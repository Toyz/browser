//! `crypto`: real randomness (`getRandomValues`, `randomUUID`) and
//! `crypto.subtle` with ring (digests, HMAC, Ed25519), native. Keys are
//! `CryptoKey` objects holding their bytes; buffers go to ring and back
//! as they are.

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::builtins::{AlignedVec, JsArray, JsArrayBuffer, JsPromise};
use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, Trace};
use ring::rand::SecureRandom;
use ring::signature::KeyPair;

use crate::dom::js_value_string;

/// A `DOMException` (the page's class) named `name`.
fn dom_error(name: &str, message: &str, cx: &mut Context) -> JsError {
    let class = cx.global_object().get(js_string!("DOMException"), cx).ok().and_then(|c| c.as_constructor());
    match class.map(|c| c.construct(&[JsString::from(message).into(), JsString::from(name).into()], None, cx)) {
        Some(Ok(e)) => JsError::from_opaque(e.into()),
        _ => JsNativeError::error().with_message(format!("{name}: {message}")).into(),
    }
}

fn not_supported(what: &str, cx: &mut Context) -> JsError {
    dom_error("NotSupportedError", &format!("{what} isn't supported"), cx)
}

/// The bytes of an ArrayBuffer or a view of one.
fn bytes_of(v: &JsValue, cx: &mut Context) -> JsResult<Vec<u8>> {
    let err = || JsNativeError::typ().with_message("expected an ArrayBuffer or a view of one");
    let o = v.as_object().ok_or_else(err)?;
    if let Ok(buffer) = JsArrayBuffer::from_object(o.clone()) {
        return Ok(buffer.data().map(|d| d.to_vec()).unwrap_or_default());
    }
    let (buffer, offset, len) = view_of(&o, cx)?;
    let data = buffer.data().ok_or_else(err)?;
    Ok(data.get(offset..offset + len).ok_or_else(err)?.to_vec())
}

/// A view's buffer, byte offset and length.
fn view_of(o: &JsObject, cx: &mut Context) -> JsResult<(JsArrayBuffer, usize, usize)> {
    let err = || JsNativeError::typ().with_message("expected an ArrayBuffer view");
    let buffer = o.get(js_string!("buffer"), cx)?.as_object().ok_or_else(err)?;
    let buffer = JsArrayBuffer::from_object(buffer).map_err(|_| err())?;
    let offset = o.get(js_string!("byteOffset"), cx)?.to_index(cx)? as usize;
    let len = o.get(js_string!("byteLength"), cx)?.to_index(cx)? as usize;
    Ok((buffer, offset, len))
}

fn buffer(bytes: &[u8], cx: &mut Context) -> JsResult<JsValue> {
    Ok(JsArrayBuffer::from_byte_block(AlignedVec::from_slice(0, bytes), cx)?.into())
}

fn random(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    ring::rand::SystemRandom::new().fill(&mut buf).ok();
    buf
}

/// An algorithm's name (`"SHA-256"` or `{ name: "SHA-256" }`), upper case.
fn alg_name(v: &JsValue, cx: &mut Context) -> JsResult<String> {
    let name = match v.as_object() {
        Some(o) => o.get(js_string!("name"), cx)?,
        None => v.clone(),
    };
    Ok(if name.is_undefined() { String::new() } else { js_value_string(&name, cx).to_ascii_uppercase() })
}

/// A hash's name without the dash (`SHA256`), SHA-256 if none.
fn hash_name(v: &JsValue, cx: &mut Context) -> JsResult<String> {
    let name = alg_name(v, cx)?.replace('-', "");
    Ok(if name.is_empty() { "SHA256".to_owned() } else { name })
}

fn hmac_alg(hash: &str) -> ring::hmac::Algorithm {
    match hash {
        "SHA1" => ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
        "SHA384" => ring::hmac::HMAC_SHA384,
        "SHA512" => ring::hmac::HMAC_SHA512,
        _ => ring::hmac::HMAC_SHA256,
    }
}

/// A key: its bytes (the HMAC secret, an Ed25519 PKCS#8 document or public
/// key) and what it's for; `type`, `algorithm`, `extractable` and
/// `usages` are its properties.
#[derive(Trace, Finalize, JsData)]
struct CryptoKey {
    #[unsafe_ignore_trace]
    bytes: Vec<u8>,
    #[unsafe_ignore_trace]
    ed25519: bool,
    /// For HMAC: the hash (`SHA256`...).
    #[unsafe_ignore_trace]
    hash: String,
    #[unsafe_ignore_trace]
    extractable: bool,
    #[unsafe_ignore_trace]
    private: bool,
}

impl Class for CryptoKey {
    const NAME: &'static str = "CryptoKey";

    fn init(_: &mut ClassBuilder<'_>) -> JsResult<()> {
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(JsNativeError::typ().with_message("Illegal constructor").into())
    }
}

/// A new key object.
fn key(data: CryptoKey, kind: &str, algorithm: JsObject, usages: Vec<String>, cx: &mut Context) -> JsResult<JsValue> {
    let extractable = data.extractable;
    let proto = cx.get_global_class::<CryptoKey>().map(|c| c.prototype());
    let o = JsObject::from_proto_and_data(proto, data);
    let usages = JsArray::from_iter(usages.into_iter().map(|u| JsValue::from(JsString::from(u))), cx);
    o.set(js_string!("type"), JsString::from(kind), false, cx)?;
    o.set(js_string!("algorithm"), algorithm, false, cx)?;
    o.set(js_string!("extractable"), extractable, false, cx)?;
    o.set(js_string!("usages"), usages, false, cx)?;
    Ok(o.into())
}

fn ed25519_algorithm(cx: &mut Context) -> JsObject {
    ObjectInitializer::new(cx).property(js_string!("name"), js_string!("Ed25519"), Attribute::all()).build()
}

fn hmac_algorithm(hash: &str, length: usize, cx: &mut Context) -> JsObject {
    // `SHA256` back to `SHA-256`.
    let dashed = if hash == "SHA1" { "SHA-1".to_owned() } else { format!("SHA-{}", &hash[3..]) };
    let hash = ObjectInitializer::new(cx).property(js_string!("name"), JsString::from(dashed), Attribute::all()).build();
    ObjectInitializer::new(cx)
        .property(js_string!("name"), js_string!("HMAC"), Attribute::all())
        .property(js_string!("hash"), hash, Attribute::all())
        .property(js_string!("length"), length, Attribute::all())
        .build()
}

fn usages_of(v: &JsValue, cx: &mut Context) -> JsResult<Vec<String>> {
    let Some(list) = v.as_object().and_then(|o| JsArray::from_object(o).ok()) else { return Ok(Vec::new()) };
    (0..list.length(cx)?).map(|i| Ok(js_value_string(&list.get(i, cx)?, cx))).collect()
}

/// Run `f` with the key argument `v`'s data.
fn with_key<R>(v: &JsValue, f: impl FnOnce(&CryptoKey) -> R) -> JsResult<R> {
    let o = v.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a CryptoKey"))?;
    let k = o.downcast_ref::<CryptoKey>().ok_or_else(|| JsNativeError::typ().with_message("not a CryptoKey"))?;
    Ok(f(&k))
}

fn arg(a: &[JsValue], i: usize) -> JsValue {
    a.get(i).cloned().unwrap_or_default()
}

type Method = fn(&[JsValue], &mut Context) -> JsResult<JsValue>;

/// `crypto.subtle`'s methods: each returns a promise (a thrown error
/// rejects it).
const SUBTLE: &[(&str, usize, Method)] = &[
    ("digest", 2, |a, cx| {
        let hash = hash_name(&arg(a, 0), cx)?;
        let alg = match hash.as_str() {
            "SHA1" => &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
            "SHA256" => &ring::digest::SHA256,
            "SHA384" => &ring::digest::SHA384,
            "SHA512" => &ring::digest::SHA512,
            _ => return Err(not_supported(&format!("digest {hash}"), cx)),
        };
        let data = bytes_of(&arg(a, 1), cx)?;
        buffer(ring::digest::digest(alg, &data).as_ref(), cx)
    }),
    ("generateKey", 3, |a, cx| {
        let alg = arg(a, 0);
        let name = alg_name(&alg, cx)?;
        let extractable = arg(a, 1).to_boolean();
        let usages = usages_of(&arg(a, 2), cx)?;
        match name.as_str() {
            "ED25519" => {
                let rng = ring::rand::SystemRandom::new();
                let failed = || JsNativeError::error().with_message("key generation failed");
                let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| failed())?;
                let public = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| failed())?.public_key().as_ref().to_vec();
                let pick = |u: &str| usages.iter().filter(|x| *x == u).cloned().collect::<Vec<_>>();
                let private = CryptoKey { bytes: pkcs8.as_ref().to_vec(), ed25519: true, hash: String::new(), extractable, private: true };
                let public = CryptoKey { bytes: public, ed25519: true, hash: String::new(), extractable: true, private: false };
                let alg = ed25519_algorithm(cx);
                let private = key(private, "private", alg.clone(), pick("sign"), cx)?;
                let public = key(public, "public", alg, pick("verify"), cx)?;
                Ok(ObjectInitializer::new(cx)
                    .property(js_string!("privateKey"), private, Attribute::all())
                    .property(js_string!("publicKey"), public, Attribute::all())
                    .build()
                    .into())
            }
            "HMAC" => {
                let o = alg.as_object().ok_or_else(|| JsNativeError::typ().with_message("HMAC needs its hash"))?;
                let hash = hash_name(&o.get(js_string!("hash"), cx)?, cx)?;
                let length = o.get(js_string!("length"), cx)?;
                let length = if length.is_undefined() {
                    if matches!(hash.as_str(), "SHA384" | "SHA512") { 1024 } else { 512 }
                } else {
                    length.to_index(cx)? as usize
                };
                let data = CryptoKey { bytes: random(length / 8), ed25519: false, hash: hash.clone(), extractable, private: false };
                let algorithm = hmac_algorithm(&hash, length, cx);
                key(data, "secret", algorithm, usages, cx)
            }
            _ => Err(not_supported(&name, cx)),
        }
    }),
    ("importKey", 5, |a, cx| {
        let format = js_value_string(&arg(a, 0), cx);
        let alg = arg(a, 2);
        let name = alg_name(&alg, cx)?;
        let extractable = arg(a, 3).to_boolean();
        let usages = usages_of(&arg(a, 4), cx)?;
        if format == "jwk" {
            return Err(not_supported("jwk keys", cx));
        }
        let raw = bytes_of(&arg(a, 1), cx)?;
        match (name.as_str(), format.as_str()) {
            ("HMAC", _) => {
                let hash = match alg.as_object() {
                    Some(o) => hash_name(&o.get(js_string!("hash"), cx)?, cx)?,
                    None => "SHA256".to_owned(),
                };
                let length = raw.len() * 8;
                let algorithm = hmac_algorithm(&hash, length, cx);
                key(CryptoKey { bytes: raw, ed25519: false, hash, extractable, private: false }, "secret", algorithm, usages, cx)
            }
            ("ED25519", "raw" | "spki") => {
                // SPKI: the key is the last 32 bytes.
                let bytes = if format == "spki" { raw[raw.len().saturating_sub(32)..].to_vec() } else { raw };
                let algorithm = ed25519_algorithm(cx);
                key(CryptoKey { bytes, ed25519: true, hash: String::new(), extractable: true, private: false }, "public", algorithm, usages, cx)
            }
            ("ED25519", "pkcs8") => {
                ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(&raw).map_err(|_| JsNativeError::typ().with_message("invalid Ed25519 private key"))?;
                let algorithm = ed25519_algorithm(cx);
                key(CryptoKey { bytes: raw, ed25519: true, hash: String::new(), extractable, private: true }, "private", algorithm, usages, cx)
            }
            _ => Err(not_supported(&format!("{name} {format} keys"), cx)),
        }
    }),
    ("exportKey", 2, |a, cx| {
        let format = js_value_string(&arg(a, 0), cx);
        let (extractable, ed25519, private, bytes) = with_key(&arg(a, 1), |k| (k.extractable, k.ed25519, k.private, k.bytes.clone()))?;
        if !extractable {
            return Err(dom_error("InvalidAccessError", "key is not extractable", cx));
        }
        match format.as_str() {
            // An Ed25519 private key's raw form is its public key.
            "raw" if ed25519 && private => {
                let pair = ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(&bytes).map_err(|_| JsNativeError::typ().with_message("invalid Ed25519 private key"))?;
                buffer(pair.public_key().as_ref(), cx)
            }
            "raw" | "pkcs8" => buffer(&bytes, cx),
            _ => Err(not_supported(&format!("{format} export"), cx)),
        }
    }),
    ("sign", 3, |a, cx| {
        let name = alg_name(&arg(a, 0), cx)?;
        let (bytes, hash) = with_key(&arg(a, 1), |k| (k.bytes.clone(), k.hash.clone()))?;
        let data = bytes_of(&arg(a, 2), cx)?;
        match name.as_str() {
            "ED25519" => {
                let pair = ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(&bytes).map_err(|_| JsNativeError::typ().with_message("invalid Ed25519 private key"))?;
                buffer(pair.sign(&data).as_ref(), cx)
            }
            "HMAC" => buffer(ring::hmac::sign(&ring::hmac::Key::new(hmac_alg(&hash), &bytes), &data).as_ref(), cx),
            _ => Err(not_supported(&name, cx)),
        }
    }),
    ("verify", 4, |a, cx| {
        let name = alg_name(&arg(a, 0), cx)?;
        let (bytes, hash) = with_key(&arg(a, 1), |k| (k.bytes.clone(), k.hash.clone()))?;
        let signature = bytes_of(&arg(a, 2), cx)?;
        let data = bytes_of(&arg(a, 3), cx)?;
        let ok = match name.as_str() {
            "ED25519" => ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &bytes).verify(&data, &signature).is_ok(),
            "HMAC" => ring::hmac::verify(&ring::hmac::Key::new(hmac_alg(&hash), &bytes), &data, &signature).is_ok(),
            _ => return Err(not_supported(&name, cx)),
        };
        Ok(ok.into())
    }),
    ("encrypt", 3, |_, cx| Err(not_supported("encrypt", cx))),
    ("decrypt", 3, |_, cx| Err(not_supported("decrypt", cx))),
    ("deriveKey", 5, |_, cx| Err(not_supported("deriveKey", cx))),
    ("deriveBits", 3, |_, cx| Err(not_supported("deriveBits", cx))),
    ("wrapKey", 4, |_, cx| Err(not_supported("wrapKey", cx))),
    ("unwrapKey", 7, |_, cx| Err(not_supported("unwrapKey", cx))),
];

/// `getRandomValues(view)`: filled with random bytes, in place.
fn get_random_values(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let view = arg(a, 0);
    let o = view.as_object().ok_or_else(|| JsNativeError::typ().with_message("getRandomValues needs an integer array"))?;
    let (buffer, offset, len) = view_of(&o, cx)?;
    if len > 65536 {
        return Err(dom_error("QuotaExceededError", "more than 65536 bytes asked for", cx));
    }
    let bytes = random(len);
    if let Some(mut data) = buffer.data_mut()
        && let Some(target) = data.get_mut(offset..offset + len)
    {
        target.copy_from_slice(&bytes);
    }
    Ok(view)
}

/// `randomUUID()`: a version 4 UUID.
fn random_uuid(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let mut b = random(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let uuid = format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]);
    Ok(JsString::from(uuid).into())
}

/// Install `crypto` and `CryptoKey` on the global object.
pub(crate) fn install(cx: &mut Context) {
    if let Err(e) = cx.register_global_class::<CryptoKey>() {
        eprintln!("js: crypto: {e}");
    }
    let mut subtle = Vec::new();
    for &(name, len, f) in SUBTLE {
        let native = NativeFunction::from_copy_closure(move |_, a, cx| {
            let promise = match f(a, cx) {
                Ok(v) => JsPromise::resolve(v, cx),
                Err(e) => JsPromise::reject(e, cx),
            };
            promise.map(JsValue::from)
        });
        subtle.push((name, FunctionObjectBuilder::new(cx.realm(), native).name(JsString::from(name)).length(len).build()));
    }
    let mut init = ObjectInitializer::new(cx);
    for (name, f) in subtle {
        init.property(JsString::from(name), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
    }
    let subtle = init.build();
    let crypto = ObjectInitializer::new(cx)
        .property(js_string!("subtle"), subtle, Attribute::CONFIGURABLE)
        .function(NativeFunction::from_fn_ptr(get_random_values), js_string!("getRandomValues"), 1)
        .function(NativeFunction::from_fn_ptr(random_uuid), js_string!("randomUUID"), 0)
        .build();
    cx.register_global_property(js_string!("crypto"), crypto, Attribute::WRITABLE | Attribute::CONFIGURABLE).ok();
}
