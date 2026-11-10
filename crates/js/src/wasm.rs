//! WebAssembly's JS API on [wasmtime]: `Module`, `Instance`, `Memory`,
//! `Table`, `Global`, the error classes, and `validate`, `compile`,
//! `instantiate` (and their `Streaming` forms). Code is compiled to native
//! code; each context (a page, a worker) has one store.
//!
//! Calls go both ways: an export is a JS function calling into the store;
//! an imported JS function is a host function calling back into JS (which
//! may call exports again: then the active `Caller` is the store). A
//! memory's `buffer` is an `ArrayBuffer` over its bytes themselves (no
//! copy): memories never move (wasmtime reserves their address space), and
//! growing one detaches the old buffer, as the spec has it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::{JsArray, JsArrayBuffer, JsFunction, JsPromise};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsBigInt, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};
use wasmtime::{AsContextMut, Caller, Engine, Extern, ExternType, Func, Instance, Memory, MemoryType, Module, Ref, RefType, Store, StoreContextMut, Table, TableType, Val, ValType};

/// What the store's host functions need.
#[derive(Default)]
struct Host {
    /// The context of the outermost call into the store.
    cx: Option<std::ptr::NonNull<Context>>,
    /// JS functions given as imports (host functions refer to them by index).
    imports: Vec<JsObject>,
    /// A JS exception an import threw, for the call that trapped on it.
    thrown: Option<JsError>,
}

/// A context's WebAssembly: its engine and store, and which JS functions
/// are wasm functions.
struct Wasm {
    engine: Engine,
    store: RefCell<Store<Host>>,
    funcs: RefCell<HashMap<JsObject, Func>>,
}

thread_local! {
    /// The `Caller` of the host function running now (null: none): the
    /// store, while it's borrowed by the call that got here.
    static ACTIVE: Cell<*mut Caller<'static, Host>> = const { Cell::new(std::ptr::null_mut()) };
}

fn wasm(cx: &mut Context) -> &Wasm {
    if cx.get_data::<Wasm>().is_none() {
        let mut config = wasmtime::Config::new();
        // A memory's bytes stay put: JS buffers point at them.
        config.memory_may_move(false);
        let engine = Engine::new(&config).expect("a wasm engine");
        let store = Store::new(&engine, Host::default());
        cx.insert_data(Wasm { engine, store: RefCell::new(store), funcs: RefCell::default() });
    }
    cx.get_data::<Wasm>().expect("inserted")
}

/// Run `f` with the store: the active caller's inside a host function,
/// else the context's (noting the context for the host functions).
fn with_store<R>(cx: &mut Context, f: impl FnOnce(StoreContextMut<'_, Host>, &mut Context) -> R) -> R {
    let active = ACTIVE.with(Cell::get);
    if !active.is_null() {
        // SAFETY: set by `host_call` for the duration of its JS call, during
        // which nothing else uses that caller.
        let caller = unsafe { &mut *active };
        return f(caller.as_context_mut(), cx);
    }
    let w: *const Wasm = wasm(cx);
    // SAFETY: the context data outlives this call (it's never removed).
    let w = unsafe { &*w };
    let mut store = w.store.borrow_mut();
    let outer = store.data_mut().cx.replace(std::ptr::NonNull::from(&mut *cx));
    let r = f(store.as_context_mut(), cx);
    store.data_mut().cx = outer;
    r
}

fn engine(cx: &mut Context) -> Engine {
    wasm(cx).engine.clone()
}

// ---- errors -------------------------------------------------------------

/// `WebAssembly.CompileError`, `LinkError`, `RuntimeError`: Errors.
#[derive(Trace, Finalize, JsData)]
struct WasmError<const K: u8> {
    #[unsafe_ignore_trace]
    message: String,
}

const ERROR_NAMES: [&str; 3] = ["CompileError", "LinkError", "RuntimeError"];

impl<const K: u8> Class for WasmError<K> {
    const NAME: &'static str = ERROR_NAMES[K as usize];
    const LENGTH: usize = 1;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        class.property(js_string!("name"), JsString::from(Self::NAME), Attribute::WRITABLE | Attribute::CONFIGURABLE);
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let message = match a.first() {
            Some(m) if !m.is_undefined() => m.to_string(cx)?.to_std_string_escaped(),
            _ => String::new(),
        };
        Ok(WasmError { message })
    }

    fn object_constructor(instance: &JsObject<Self>, _: &[JsValue], cx: &mut Context) -> JsResult<()> {
        let message = instance.borrow().data().message.clone();
        let o: JsObject = instance.clone().upcast();
        o.set(js_string!("message"), JsString::from(message), false, cx)?;
        Ok(())
    }
}

type CompileError = WasmError<0>;
type LinkError = WasmError<1>;
type RuntimeError = WasmError<2>;

fn error<const K: u8>(message: impl std::fmt::Display, cx: &mut Context) -> JsError {
    let message = message.to_string();
    match WasmError::<K>::from_data(WasmError { message: message.clone() }, cx) {
        Ok(o) => {
            o.set(js_string!("message"), JsString::from(message), false, cx).ok();
            JsError::from_opaque(o.into())
        }
        Err(e) => e,
    }
}

/// What a failed call into the store throws: the JS exception an import
/// threw, else a `RuntimeError` (a trap).
fn call_error(e: wasmtime::Error, thrown: Option<JsError>, cx: &mut Context) -> JsError {
    thrown.unwrap_or_else(|| error::<2>(format!("{e:#}"), cx))
}

// ---- values -------------------------------------------------------------

/// A wasm value as JS sees it.
fn to_js(v: &Val, cx: &mut Context) -> JsValue {
    match v {
        Val::I32(n) => JsValue::from(*n),
        Val::I64(n) => JsBigInt::from(*n).into(),
        Val::F32(bits) => JsValue::from(f64::from(f32::from_bits(*bits))),
        Val::F64(bits) => JsValue::from(f64::from_bits(*bits)),
        Val::FuncRef(Some(f)) => function(*f, cx).into(),
        _ => JsValue::null(),
    }
}

/// A JS value as a wasm value of type `ty`.
fn to_wasm(v: &JsValue, ty: &ValType, cx: &mut Context) -> JsResult<Val> {
    Ok(match ty {
        ValType::I32 => Val::I32(v.to_i32(cx)?),
        ValType::I64 => Val::I64(v.to_big_int64(cx)?),
        ValType::F32 => Val::F32((v.to_number(cx)? as f32).to_bits()),
        ValType::F64 => Val::F64(v.to_number(cx)?.to_bits()),
        ValType::Ref(r) if r.heap_type().is_func() => Val::FuncRef(wasm_func(v, cx)),
        ValType::Ref(_) => Val::null_extern_ref(),
        ValType::V128 => return Err(JsNativeError::typ().with_message("WebAssembly: v128 values can't cross to JS").into()),
    })
}

/// The wasm function a JS value is (one of ours), if it is.
fn wasm_func(v: &JsValue, cx: &mut Context) -> Option<Func> {
    let o = v.as_object()?;
    wasm(cx).funcs.borrow().get(&o).copied()
}

/// A zero of type `ty` (results to be filled).
fn zero(ty: &ValType) -> Val {
    match ty {
        ValType::I32 => Val::I32(0),
        ValType::I64 => Val::I64(0),
        ValType::F32 => Val::F32(0),
        ValType::F64 => Val::F64(0),
        ValType::V128 => Val::V128(0u128.into()),
        ValType::Ref(_) => Val::FuncRef(None),
    }
}

/// Call `func` with JS arguments: its results (none: undefined; several:
/// an array).
fn call(func: Func, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let ty = with_store(cx, |store, _| func.ty(&store));
    let mut params = Vec::with_capacity(ty.params().len());
    for (i, t) in ty.params().enumerate() {
        params.push(to_wasm(&args.get(i).cloned().unwrap_or_default(), &t, cx)?);
    }
    let mut results: Vec<Val> = ty.results().map(|t| zero(&t)).collect();
    let outcome = with_store(cx, |mut store, _| {
        let r = func.call(&mut store, &params, &mut results);
        r.map_err(|e| (e, store.data_mut().thrown.take()))
    });
    if let Err((e, thrown)) = outcome {
        return Err(call_error(e, thrown, cx));
    }
    Ok(match results.as_slice() {
        [] => JsValue::undefined(),
        [one] => to_js(one, cx),
        many => {
            let values: Vec<JsValue> = many.iter().map(|v| to_js(v, cx)).collect();
            JsArray::from_iter(values, cx).into()
        }
    })
}

/// A JS function calling wasm function `func` (and known as it).
fn function(func: Func, cx: &mut Context) -> JsFunction {
    let native = NativeFunction::from_copy_closure(move |_, args, cx| call(func, args, cx));
    let f = FunctionObjectBuilder::new(cx.realm(), native).name(js_string!("wasm-function")).build();
    let o: JsObject = f.clone().into();
    wasm(cx).funcs.borrow_mut().insert(o, func);
    f
}

/// A host function for JS function `f` (an import of type `ty`).
fn host_function(f: JsObject, ty: wasmtime::FuncType, store: &mut StoreContextMut<'_, Host>) -> Func {
    let index = store.data().imports.len();
    store.data_mut().imports.push(f);
    let result_types: Vec<ValType> = ty.results().collect();
    Func::new(store.as_context_mut(), ty, move |mut caller: Caller<'_, Host>, params, results| {
        let Some(cx) = caller.data().cx else { return Err(wasmtime::Error::msg("WebAssembly: no context")) };
        // SAFETY: the context of the call into the store that got here, which
        // is suspended in it.
        let cx = unsafe { &mut *cx.as_ptr() };
        let f = caller.data().imports[index].clone();
        let args: Vec<JsValue> = params.iter().map(|v| to_js(v, cx)).collect();
        // While JS runs, this caller is the store.
        let me = std::ptr::addr_of_mut!(caller).cast::<Caller<'static, Host>>();
        let outer = ACTIVE.with(|a| a.replace(me));
        let r = f.call(&JsValue::undefined(), &args, cx);
        ACTIVE.with(|a| a.set(outer));
        let value = match r {
            Ok(v) => v,
            Err(e) => {
                caller.data_mut().thrown = Some(e);
                return Err(wasmtime::Error::msg("a JS import threw"));
            }
        };
        let converted = (|| -> JsResult<()> {
            match result_types.as_slice() {
                [] => {}
                [t] => results[0] = to_wasm(&value, t, cx)?,
                many => {
                    let array = value.as_object().ok_or_else(|| JsNativeError::typ().with_message("WebAssembly: multiple results need an array"))?;
                    for (i, t) in many.iter().enumerate() {
                        results[i] = to_wasm(&array.get(i, cx)?, t, cx)?;
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = converted {
            caller.data_mut().thrown = Some(e);
            return Err(wasmtime::Error::msg("a JS import's result didn't convert"));
        }
        Ok(())
    })
}

// ---- Module -------------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
struct WasmModule {
    #[unsafe_ignore_trace]
    module: Module,
}

fn bytes_arg(v: &JsValue, cx: &mut Context) -> JsResult<Vec<u8>> {
    crate::blob::binary_of(v, cx)?.ok_or_else(|| JsNativeError::typ().with_message("WebAssembly: expected an ArrayBuffer or typed array").into())
}

fn compile(bytes: &[u8], cx: &mut Context) -> JsResult<Module> {
    let engine = engine(cx);
    Module::new(&engine, bytes).map_err(|e| error::<0>(format!("{e:#}"), cx))
}

fn module_of(v: &JsValue) -> Option<Module> {
    v.as_object().and_then(|o| o.downcast_ref::<WasmModule>().map(|m| m.module.clone()))
}

fn kind_name(t: &ExternType) -> &'static str {
    match t {
        ExternType::Func(_) => "function",
        ExternType::Table(_) => "table",
        ExternType::Memory(_) => "memory",
        ExternType::Global(_) => "global",
        _ => "tag",
    }
}

fn descriptor(pairs: &[(&str, JsValue)], cx: &mut Context) -> JsValue {
    let o = JsObject::with_object_proto(cx.intrinsics());
    for (k, v) in pairs {
        o.set(JsString::from(*k), v.clone(), false, cx).ok();
    }
    o.into()
}

impl Class for WasmModule {
    const NAME: &'static str = "Module";
    const LENGTH: usize = 1;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        class.static_method(js_string!("exports"), 1, NativeFunction::from_fn_ptr(|_, a, cx| {
            let m = module_of(&a.first().cloned().unwrap_or_default()).ok_or_else(|| JsNativeError::typ().with_message("not a WebAssembly.Module"))?;
            let list: Vec<JsValue> = m.exports().map(|e| descriptor(&[("name", JsString::from(e.name()).into()), ("kind", JsString::from(kind_name(&e.ty())).into())], cx)).collect();
            Ok(JsArray::from_iter(list, cx).into())
        }));
        class.static_method(js_string!("imports"), 1, NativeFunction::from_fn_ptr(|_, a, cx| {
            let m = module_of(&a.first().cloned().unwrap_or_default()).ok_or_else(|| JsNativeError::typ().with_message("not a WebAssembly.Module"))?;
            let list: Vec<JsValue> = m
                .imports()
                .map(|i| descriptor(&[("module", JsString::from(i.module()).into()), ("name", JsString::from(i.name()).into()), ("kind", JsString::from(kind_name(&i.ty())).into())], cx))
                .collect();
            Ok(JsArray::from_iter(list, cx).into())
        }));
        class.static_method(js_string!("customSections"), 2, NativeFunction::from_fn_ptr(|_, _, cx| Ok(JsArray::new(cx)?.into())));
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let bytes = bytes_arg(&a.first().cloned().unwrap_or_default(), cx)?;
        Ok(WasmModule { module: compile(&bytes, cx)? })
    }
}

// ---- Instance -----------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
struct WasmInstance {
    exports: JsObject,
}

/// `importObject[module][name]` for each of the module's imports, as externs.
fn resolve_imports(module: &Module, imports: &JsValue, cx: &mut Context) -> JsResult<Vec<Extern>> {
    let mut out = Vec::new();
    for import in module.imports() {
        let ns = imports.as_object().ok_or_else(|| JsNativeError::typ().with_message("WebAssembly: imports needed but no import object given"))?;
        let ns = ns.get(JsString::from(import.module()), cx)?;
        let ns = ns.as_object().ok_or_else(|| error::<1>(format!("import module {:?} isn't an object", import.module()), cx))?;
        let v = ns.get(JsString::from(import.name()), cx)?;
        let what = format!("{}.{}", import.module(), import.name());
        let ext: Extern = match import.ty() {
            ExternType::Func(ty) => {
                if let Some(f) = wasm_func(&v, cx) {
                    f.into()
                } else {
                    let f = v.as_object().filter(|o| o.is_callable()).ok_or_else(|| error::<1>(format!("import {what} must be a function"), cx))?;
                    with_store(cx, |mut store, _| host_function(f, ty, &mut store)).into()
                }
            }
            ExternType::Memory(_) => v.as_object().and_then(|o| o.downcast_ref::<WasmMemory>().map(|m| m.memory)).ok_or_else(|| error::<1>(format!("import {what} must be a WebAssembly.Memory"), cx))?.into(),
            ExternType::Table(_) => v.as_object().and_then(|o| o.downcast_ref::<WasmTable>().map(|t| t.table)).ok_or_else(|| error::<1>(format!("import {what} must be a WebAssembly.Table"), cx))?.into(),
            ExternType::Global(ty) => match v.as_object().and_then(|o| o.downcast_ref::<WasmGlobal>().map(|g| g.global)) {
                Some(g) => g.into(),
                None => {
                    let val = to_wasm(&v, ty.content(), cx)?;
                    with_store(cx, |store, _| wasmtime::Global::new(store, ty, val)).map_err(|e| error::<1>(format!("import {what}: {e:#}"), cx))?.into()
                }
            },
            _ => return Err(error::<1>(format!("import {what}: unsupported kind"), cx)),
        };
        out.push(ext);
    }
    Ok(out)
}

/// Instantiate `module` with `imports`: its `Instance` object.
fn instantiate(module: &Module, imports: &JsValue, cx: &mut Context) -> JsResult<JsObject> {
    let externs = resolve_imports(module, imports, cx)?;
    let made = with_store(cx, |mut store, _| Instance::new(&mut store, module, &externs).map_err(|e| (e, store.data_mut().thrown.take())));
    let instance = match made {
        Ok(i) => i,
        Err((e, thrown)) => {
            let text = format!("{e:#}");
            return Err(match thrown {
                Some(js) => js,
                None if text.contains("unknown import") || text.contains("incompatible import") => error::<1>(text, cx),
                None => error::<2>(text, cx),
            });
        }
    };
    let exports = JsObject::with_null_proto();
    let list: Vec<(String, Extern)> = with_store(cx, |mut store, _| instance.exports(&mut store).map(|e| (e.name().to_owned(), e.into_extern())).collect());
    for (name, ext) in list {
        let v: JsValue = match ext {
            Extern::Func(f) => function(f, cx).into(),
            Extern::Memory(m) => WasmMemory::from_data(WasmMemory::of(m), cx)?.into(),
            Extern::Table(t) => WasmTable::from_data(WasmTable { table: t }, cx)?.into(),
            Extern::Global(g) => WasmGlobal::from_data(WasmGlobal { global: g }, cx)?.into(),
            _ => JsValue::undefined(),
        };
        exports.set(JsString::from(name), v, false, cx)?;
    }
    WasmInstance::from_data(WasmInstance { exports }, cx)
}

impl Class for WasmInstance {
    const NAME: &'static str = "Instance";
    const LENGTH: usize = 1;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        let get = NativeFunction::from_fn_ptr(|this, _, _| {
            let o = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a WebAssembly.Instance"))?;
            let i = o.downcast_ref::<WasmInstance>().ok_or_else(|| JsNativeError::typ().with_message("not a WebAssembly.Instance"))?;
            Ok(i.exports.clone().into())
        });
        let realm = class.context().realm().clone();
        let get = FunctionObjectBuilder::new(&realm, get).name(js_string!("get exports")).build();
        class.accessor(js_string!("exports"), Some(get), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let module = module_of(&a.first().cloned().unwrap_or_default()).ok_or_else(|| JsNativeError::typ().with_message("WebAssembly.Instance needs a Module"))?;
        let made = instantiate(&module, &a.get(1).cloned().unwrap_or_default(), cx)?;
        let exports = made.downcast_ref::<WasmInstance>().expect("an instance").exports.clone();
        Ok(WasmInstance { exports })
    }
}

// ---- Memory -------------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
struct WasmMemory {
    #[unsafe_ignore_trace]
    memory: Memory,
    /// The current `buffer`, and how long it was made.
    buffer: GcRefCell<Option<JsObject>>,
    #[unsafe_ignore_trace]
    length: Cell<usize>,
}

impl WasmMemory {
    fn of(memory: Memory) -> Self {
        WasmMemory { memory, buffer: GcRefCell::new(None), length: Cell::new(usize::MAX) }
    }
}

/// What detaches a memory's buffers (only we may).
fn detach_key() -> JsValue {
    js_string!("WebAssembly.Memory").into()
}

fn memory_this(this: &JsValue) -> JsResult<JsObject> {
    this.as_object().filter(|o| o.downcast_ref::<WasmMemory>().is_some()).ok_or_else(|| JsNativeError::typ().with_message("not a WebAssembly.Memory").into())
}

/// The memory's buffer: the one made at its current size, or a new one
/// (the old one detached).
fn buffer(o: &JsObject, cx: &mut Context) -> JsResult<JsValue> {
    let memory = o.downcast_ref::<WasmMemory>().expect("a memory").memory;
    let (ptr, len) = with_store(cx, |store, _| (memory.data_ptr(&store), memory.data_size(&store)));
    {
        let m = o.downcast_ref::<WasmMemory>().expect("a memory");
        if m.length.get() == len
            && let Some(b) = m.buffer.borrow().as_ref()
        {
            return Ok(b.clone().into());
        }
    }
    let old = o.downcast_ref::<WasmMemory>().expect("a memory").buffer.borrow_mut().take();
    if let Some(old) = old
        && let Ok(old) = JsArrayBuffer::from_object(old)
    {
        old.detach(&detach_key()).ok();
    }
    let ptr = std::ptr::NonNull::new(ptr).unwrap_or(std::ptr::NonNull::dangling());
    // SAFETY: memories never move (`memory_may_move(false)`) and live as long
    // as the store, which lives as long as the context.
    let bytes = unsafe { boa_engine::builtins::array_buffer::ExternalBytes::new(ptr, len, Rc::new(())) };
    let buffer = JsArrayBuffer::external(bytes, detach_key(), cx);
    let b: JsObject = buffer.into();
    let m = o.downcast_ref::<WasmMemory>().expect("a memory");
    *m.buffer.borrow_mut() = Some(b.clone());
    m.length.set(len);
    Ok(b.into())
}

fn descriptor_u32(d: &JsObject, key: &str, cx: &mut Context) -> JsResult<Option<u32>> {
    let v = d.get(JsString::from(key), cx)?;
    if v.is_undefined() { Ok(None) } else { Ok(Some(v.to_u32(cx)?)) }
}

impl Class for WasmMemory {
    const NAME: &'static str = "Memory";
    const LENGTH: usize = 1;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        let get = NativeFunction::from_fn_ptr(|this, _, cx| buffer(&memory_this(this)?, cx));
        let realm = class.context().realm().clone();
        let get = FunctionObjectBuilder::new(&realm, get).name(js_string!("get buffer")).build();
        class.accessor(js_string!("buffer"), Some(get), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        class.method(js_string!("grow"), 1, NativeFunction::from_fn_ptr(|this, a, cx| {
            let o = memory_this(this)?;
            let delta = a.first().cloned().unwrap_or_default().to_u32(cx)?;
            let memory = o.downcast_ref::<WasmMemory>().expect("checked").memory;
            let old = with_store(cx, |store, _| memory.grow(store, u64::from(delta)));
            let old = old.map_err(|e| JsNativeError::range().with_message(format!("WebAssembly.Memory.grow: {e:#}")))?;
            // The old buffer goes now, even when nothing grew.
            o.downcast_ref::<WasmMemory>().expect("checked").length.set(usize::MAX);
            buffer(&o, cx)?;
            Ok(JsValue::from(old as f64))
        }));
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let d = a.first().and_then(JsValue::as_object).ok_or_else(|| JsNativeError::typ().with_message("WebAssembly.Memory needs a descriptor"))?;
        let initial = descriptor_u32(&d, "initial", cx)?.unwrap_or(0);
        let maximum = descriptor_u32(&d, "maximum", cx)?;
        let memory = with_store(cx, |store, _| Memory::new(store, MemoryType::new(initial, maximum)));
        let memory = memory.map_err(|e| JsNativeError::range().with_message(format!("WebAssembly.Memory: {e:#}")))?;
        Ok(WasmMemory::of(memory))
    }
}

// ---- Table --------------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
struct WasmTable {
    #[unsafe_ignore_trace]
    table: Table,
}

fn table_this(this: &JsValue) -> JsResult<Table> {
    this.as_object().and_then(|o| o.downcast_ref::<WasmTable>().map(|t| t.table)).ok_or_else(|| JsNativeError::typ().with_message("not a WebAssembly.Table").into())
}

fn to_ref(v: &JsValue, cx: &mut Context) -> JsResult<Ref> {
    if v.is_null_or_undefined() {
        return Ok(Ref::Func(None));
    }
    wasm_func(v, cx).map(|f| Ref::Func(Some(f))).ok_or_else(|| JsNativeError::typ().with_message("WebAssembly.Table: only wasm functions go in a table").into())
}

impl Class for WasmTable {
    const NAME: &'static str = "Table";
    const LENGTH: usize = 1;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        let get = NativeFunction::from_fn_ptr(|this, _, cx| {
            let t = table_this(this)?;
            Ok(JsValue::from(with_store(cx, |store, _| t.size(&store)) as f64))
        });
        let realm = class.context().realm().clone();
        let get = FunctionObjectBuilder::new(&realm, get).name(js_string!("get length")).build();
        class.accessor(js_string!("length"), Some(get), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        class.method(js_string!("get"), 1, NativeFunction::from_fn_ptr(|this, a, cx| {
            let t = table_this(this)?;
            let i = a.first().cloned().unwrap_or_default().to_u32(cx)?;
            match with_store(cx, |store, _| t.get(store, u64::from(i))) {
                Some(Ref::Func(Some(f))) => Ok(function(f, cx).into()),
                Some(_) => Ok(JsValue::null()),
                None => Err(JsNativeError::range().with_message("WebAssembly.Table.get: index out of range").into()),
            }
        }));
        class.method(js_string!("set"), 2, NativeFunction::from_fn_ptr(|this, a, cx| {
            let t = table_this(this)?;
            let i = a.first().cloned().unwrap_or_default().to_u32(cx)?;
            let r = to_ref(&a.get(1).cloned().unwrap_or_default(), cx)?;
            with_store(cx, |store, _| t.set(store, u64::from(i), r)).map_err(|e| JsNativeError::range().with_message(format!("WebAssembly.Table.set: {e:#}")))?;
            Ok(JsValue::undefined())
        }));
        class.method(js_string!("grow"), 1, NativeFunction::from_fn_ptr(|this, a, cx| {
            let t = table_this(this)?;
            let delta = a.first().cloned().unwrap_or_default().to_u32(cx)?;
            let init = to_ref(&a.get(1).cloned().unwrap_or_default(), cx)?;
            let old = with_store(cx, |store, _| t.grow(store, u64::from(delta), init)).map_err(|e| JsNativeError::range().with_message(format!("WebAssembly.Table.grow: {e:#}")))?;
            Ok(JsValue::from(old as f64))
        }));
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let d = a.first().and_then(JsValue::as_object).ok_or_else(|| JsNativeError::typ().with_message("WebAssembly.Table needs a descriptor"))?;
        let initial = descriptor_u32(&d, "initial", cx)?.unwrap_or(0);
        let maximum = descriptor_u32(&d, "maximum", cx)?;
        let ty = TableType::new(RefType::FUNCREF, initial, maximum);
        let table = with_store(cx, |store, _| Table::new(store, ty, Ref::Func(None)));
        Ok(WasmTable { table: table.map_err(|e| JsNativeError::range().with_message(format!("WebAssembly.Table: {e:#}")))? })
    }
}

// ---- Global -------------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
struct WasmGlobal {
    #[unsafe_ignore_trace]
    global: wasmtime::Global,
}

fn global_this(this: &JsValue) -> JsResult<wasmtime::Global> {
    this.as_object().and_then(|o| o.downcast_ref::<WasmGlobal>().map(|g| g.global)).ok_or_else(|| JsNativeError::typ().with_message("not a WebAssembly.Global").into())
}

fn global_value(this: &JsValue, cx: &mut Context) -> JsResult<JsValue> {
    let g = global_this(this)?;
    let v = with_store(cx, |store, _| g.get(store));
    Ok(to_js(&v, cx))
}

impl Class for WasmGlobal {
    const NAME: &'static str = "Global";
    const LENGTH: usize = 1;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        let realm = class.context().realm().clone();
        let get = FunctionObjectBuilder::new(&realm, NativeFunction::from_fn_ptr(|this, _, cx| global_value(this, cx))).name(js_string!("get value")).build();
        let set = NativeFunction::from_fn_ptr(|this, a, cx| {
            let g = global_this(this)?;
            let ty = with_store(cx, |store, _| g.ty(&store));
            let v = to_wasm(&a.first().cloned().unwrap_or_default(), ty.content(), cx)?;
            with_store(cx, |store, _| g.set(store, v)).map_err(|e| JsNativeError::typ().with_message(format!("WebAssembly.Global: {e:#}")))?;
            Ok(JsValue::undefined())
        });
        let set = FunctionObjectBuilder::new(&realm, set).name(js_string!("set value")).build();
        class.accessor(js_string!("value"), Some(get), Some(set), Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        class.method(js_string!("valueOf"), 0, NativeFunction::from_fn_ptr(|this, _, cx| global_value(this, cx)));
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let d = a.first().and_then(JsValue::as_object).ok_or_else(|| JsNativeError::typ().with_message("WebAssembly.Global needs a descriptor"))?;
        let kind = d.get(js_string!("value"), cx)?.to_string(cx)?.to_std_string_escaped();
        let ty = match kind.as_str() {
            "i32" => ValType::I32,
            "i64" => ValType::I64,
            "f32" => ValType::F32,
            "f64" => ValType::F64,
            _ => return Err(JsNativeError::typ().with_message(format!("WebAssembly.Global: unsupported type {kind:?}")).into()),
        };
        let mutable = d.get(js_string!("mutable"), cx)?.to_boolean();
        let init = match a.get(1) {
            Some(v) if !v.is_undefined() => to_wasm(v, &ty, cx)?,
            _ => zero(&ty),
        };
        let gty = wasmtime::GlobalType::new(ty, if mutable { wasmtime::Mutability::Var } else { wasmtime::Mutability::Const });
        let global = with_store(cx, |store, _| wasmtime::Global::new(store, gty, init));
        Ok(WasmGlobal { global: global.map_err(|e| JsNativeError::typ().with_message(format!("WebAssembly.Global: {e:#}")))? })
    }
}

// ---- the namespace ------------------------------------------------------

fn settle(result: JsResult<JsValue>, cx: &mut Context) -> JsResult<JsValue> {
    Ok(match result {
        Ok(v) => JsPromise::resolve(v, cx)?.into(),
        Err(e) => JsPromise::reject(e, cx)?.into(),
    })
}

/// `instantiate(bytes, imports)`: `{ module, instance }`; with a Module:
/// the Instance.
fn instantiate_value(source: &JsValue, imports: &JsValue, cx: &mut Context) -> JsResult<JsValue> {
    if let Some(module) = module_of(source) {
        return Ok(instantiate(&module, imports, cx)?.into());
    }
    let bytes = bytes_arg(source, cx)?;
    let module = compile(&bytes, cx)?;
    let instance = instantiate(&module, imports, cx)?;
    let module = WasmModule::from_data(WasmModule { module }, cx)?;
    Ok(descriptor(&[("module", module.into()), ("instance", instance.into())], cx))
}

/// A `Response` (or a promise of one), read whole; then `then` with the bytes.
fn from_response(source: &JsValue, then: JsFunction, cx: &mut Context) -> JsResult<JsValue> {
    let read = NativeFunction::from_fn_ptr(|_, a, cx| {
        let r = a.first().and_then(JsValue::as_object).ok_or_else(|| JsNativeError::typ().with_message("WebAssembly: expected a Response"))?;
        let f = r.get(js_string!("arrayBuffer"), cx)?;
        let f = f.as_callable().ok_or_else(|| JsNativeError::typ().with_message("WebAssembly: expected a Response"))?;
        f.call(&r.clone().into(), &[], cx)
    });
    let read = FunctionObjectBuilder::new(cx.realm(), read).build();
    let p = JsPromise::resolve(source.clone(), cx)?.then(Some(read), None, cx)?;
    Ok(p.then(Some(then), None, cx)?.into())
}

fn namespace_fn(cx: &mut Context, ns: &JsObject, name: &str, len: usize, f: fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>) {
    let f = FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_fn_ptr(f)).name(JsString::from(name)).length(len).build();
    ns.set(JsString::from(name), f, false, cx).ok();
}

/// Register `WebAssembly`.
pub(crate) fn install(cx: &mut Context) {
    let results = [
        cx.register_global_class::<WasmModule>(),
        cx.register_global_class::<WasmInstance>(),
        cx.register_global_class::<WasmMemory>(),
        cx.register_global_class::<WasmTable>(),
        cx.register_global_class::<WasmGlobal>(),
        cx.register_global_class::<CompileError>(),
        cx.register_global_class::<LinkError>(),
        cx.register_global_class::<RuntimeError>(),
    ];
    for r in results {
        if let Err(e) = r {
            eprintln!("js: WebAssembly: {e}");
        }
    }
    let global = cx.global_object();
    let ns = JsObject::with_object_proto(cx.intrinsics());
    // The classes live on the namespace, not the global.
    for name in ["Module", "Instance", "Memory", "Table", "Global", "CompileError", "LinkError", "RuntimeError"] {
        let class = global.get(JsString::from(name), cx).unwrap_or_default();
        ns.set(JsString::from(name), class, false, cx).ok();
        global.delete_property_or_throw(JsString::from(name), cx).ok();
    }
    // The errors are Errors.
    let error_proto = cx.intrinsics().constructors().error().prototype();
    for proto in [
        cx.get_global_class::<CompileError>().map(|c| c.prototype()),
        cx.get_global_class::<LinkError>().map(|c| c.prototype()),
        cx.get_global_class::<RuntimeError>().map(|c| c.prototype()),
    ]
    .into_iter()
    .flatten()
    {
        proto.set_prototype(Some(error_proto.clone()));
    }
    namespace_fn(cx, &ns, "validate", 1, |_, a, cx| {
        let bytes = bytes_arg(&a.first().cloned().unwrap_or_default(), cx)?;
        let engine = engine(cx);
        Ok(Module::validate(&engine, &bytes).is_ok().into())
    });
    namespace_fn(cx, &ns, "compile", 1, |_, a, cx| {
        let r = (|| {
            let bytes = bytes_arg(&a.first().cloned().unwrap_or_default(), cx)?;
            let module = compile(&bytes, cx)?;
            Ok(WasmModule::from_data(WasmModule { module }, cx)?.into())
        })();
        settle(r, cx)
    });
    namespace_fn(cx, &ns, "instantiate", 1, |_, a, cx| {
        let r = instantiate_value(&a.first().cloned().unwrap_or_default(), &a.get(1).cloned().unwrap_or_default(), cx);
        settle(r, cx)
    });
    namespace_fn(cx, &ns, "compileStreaming", 1, |_, a, cx| {
        let then = FunctionObjectBuilder::new(
            cx.realm(),
            NativeFunction::from_fn_ptr(|_, a, cx| {
                let bytes = bytes_arg(&a.first().cloned().unwrap_or_default(), cx)?;
                let module = compile(&bytes, cx)?;
                Ok(WasmModule::from_data(WasmModule { module }, cx)?.into())
            }),
        )
        .build();
        from_response(&a.first().cloned().unwrap_or_default(), then, cx)
    });
    namespace_fn(cx, &ns, "instantiateStreaming", 1, |_, a, cx| {
        let imports = a.get(1).cloned().unwrap_or_default();
        let then = NativeFunction::from_copy_closure_with_captures(|_, a, imports: &JsValue, cx| instantiate_value(&a.first().cloned().unwrap_or_default(), imports, cx), imports);
        let then = FunctionObjectBuilder::new(cx.realm(), then).build();
        from_response(&a.first().cloned().unwrap_or_default(), then, cx)
    });
    global.set(js_string!("WebAssembly"), ns, false, cx).ok();
}
