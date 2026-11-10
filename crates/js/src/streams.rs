//! Streams, native: `ReadableStream` (an underlying source's `start`,
//! `pull` and `cancel`; readers; `pipeTo`, `pipeThrough`, `tee`, async
//! iteration, `from`), `WritableStream`, `TransformStream`, and the
//! encoding ones built on it (`TextDecoderStream`, `TextEncoderStream`).
//! Rust feeds streams too ([`readable`], [`push`], [`close`], [`fail`]):
//! blobs' and response bodies'.

use std::collections::VecDeque;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::{JsFunction, JsPromise};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, Gc, GcRefCell, Trace};

// ---- shared state ----------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Default)]
enum State {
    #[default]
    Readable,
    Closed,
    Errored,
}

#[derive(Trace, Finalize)]
struct Waiter {
    resolve: JsFunction,
    reject: JsFunction,
}

#[derive(Trace, Finalize, Default)]
pub(crate) struct Readable {
    queue: VecDeque<JsValue>,
    /// Reads waiting for a chunk.
    reads: Vec<Waiter>,
    /// `reader.closed` promises.
    closed: Vec<Waiter>,
    #[unsafe_ignore_trace]
    state: State,
    error: JsValue,
    /// The underlying source (`pull`, `cancel`) and its controller.
    source: Option<JsObject>,
    controller: Option<JsObject>,
    #[unsafe_ignore_trace]
    locked: bool,
    #[unsafe_ignore_trace]
    pulling: bool,
}

pub(crate) type Shared = Gc<GcRefCell<Readable>>;

/// `{ value, done }`.
fn step(value: JsValue, done: bool, cx: &mut Context) -> JsValue {
    let o = JsObject::with_object_proto(cx.intrinsics());
    o.set(js_string!("value"), value, false, cx).ok();
    o.set(js_string!("done"), done, false, cx).ok();
    o.into()
}

fn resolved(v: JsValue, cx: &mut Context) -> JsResult<JsValue> {
    Ok(JsPromise::resolve(v, cx)?.into())
}

fn rejected(e: JsValue, cx: &mut Context) -> JsResult<JsValue> {
    Ok(JsPromise::reject(JsError::from_opaque(e), cx)?.into())
}

fn type_error(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}

/// Add a chunk (to a waiting read, else the queue).
pub(crate) fn push(r: &Shared, chunk: JsValue, cx: &mut Context) {
    let waiter = {
        let mut s = r.borrow_mut();
        if s.state != State::Readable {
            return;
        }
        if s.reads.is_empty() {
            s.queue.push_back(chunk.clone());
            None
        } else {
            Some(s.reads.remove(0))
        }
    };
    if let Some(w) = waiter {
        let v = step(chunk, false, cx);
        w.resolve.call(&JsValue::undefined(), &[v], cx).ok();
    }
}

/// No more chunks (those queued are still read).
pub(crate) fn close(r: &Shared, cx: &mut Context) {
    let (reads, closed) = {
        let mut s = r.borrow_mut();
        if s.state != State::Readable {
            return;
        }
        s.state = State::Closed;
        let reads = if s.queue.is_empty() { std::mem::take(&mut s.reads) } else { Vec::new() };
        (reads, std::mem::take(&mut s.closed))
    };
    for w in reads {
        let v = step(JsValue::undefined(), true, cx);
        w.resolve.call(&JsValue::undefined(), &[v], cx).ok();
    }
    for w in closed {
        w.resolve.call(&JsValue::undefined(), &[], cx).ok();
    }
}

/// The stream failed with `e`.
pub(crate) fn fail(r: &Shared, e: JsValue, cx: &mut Context) {
    let (reads, closed) = {
        let mut s = r.borrow_mut();
        if s.state != State::Readable {
            return;
        }
        s.state = State::Errored;
        s.error = e.clone();
        s.queue.clear();
        (std::mem::take(&mut s.reads), std::mem::take(&mut s.closed))
    };
    for w in reads.into_iter().chain(closed) {
        w.reject.call(&JsValue::undefined(), std::slice::from_ref(&e), cx).ok();
    }
}

/// Ask the source for more, if it pulls and isn't already.
fn pull(r: &Shared, cx: &mut Context) {
    let (source, controller) = {
        let mut s = r.borrow_mut();
        if s.pulling || s.state != State::Readable || !s.queue.is_empty() {
            return;
        }
        let (Some(source), Some(controller)) = (s.source.clone(), s.controller.clone()) else { return };
        s.pulling = true;
        (source, controller)
    };
    let f = source.get(js_string!("pull"), cx).ok().filter(JsValue::is_callable);
    let result = f.map(|f| f.as_callable().expect("callable").call(&source.clone().into(), &[controller.into()], cx));
    match result {
        None => r.borrow_mut().pulling = false,
        Some(Err(e)) => {
            r.borrow_mut().pulling = false;
            let e = e.into_opaque(cx).unwrap_or_default();
            fail(r, e, cx);
        }
        Some(Ok(v)) => {
            // Done pulling once what it returned settles; again if reads wait.
            let again = NativeFunction::from_copy_closure_with_captures(
                |_, _, r: &Shared, cx| {
                    r.borrow_mut().pulling = false;
                    if !r.borrow().reads.is_empty() {
                        pull(r, cx);
                    }
                    Ok(JsValue::undefined())
                },
                r.clone(),
            );
            let failed = NativeFunction::from_copy_closure_with_captures(
                |_, a, r: &Shared, cx| {
                    r.borrow_mut().pulling = false;
                    fail(r, a.first().cloned().unwrap_or_default(), cx);
                    Ok(JsValue::undefined())
                },
                r.clone(),
            );
            let again = FunctionObjectBuilder::new(cx.realm(), again).build();
            let failed = FunctionObjectBuilder::new(cx.realm(), failed).build();
            if let Ok(p) = JsPromise::resolve(v, cx) {
                p.then(Some(again), Some(failed), cx).ok();
            }
        }
    }
}

/// A read: a promise of `{ value, done }`.
fn read(r: &Shared, cx: &mut Context) -> JsResult<JsValue> {
    let now = {
        let mut s = r.borrow_mut();
        if let Some(chunk) = s.queue.pop_front() {
            Some(Ok((chunk, false)))
        } else if s.state == State::Errored {
            Some(Err(s.error.clone()))
        } else if s.state == State::Closed {
            Some(Ok((JsValue::undefined(), true)))
        } else {
            None
        }
    };
    match now {
        Some(Ok((value, done))) => {
            let v = step(value, done, cx);
            let p = resolved(v, cx);
            // A closed stream whose queue just emptied: later reads are done.
            pull(r, cx);
            p
        }
        Some(Err(e)) => rejected(e, cx),
        None => {
            let (promise, f) = JsPromise::new_pending(cx);
            r.borrow_mut().reads.push(Waiter { resolve: f.resolve, reject: f.reject });
            pull(r, cx);
            Ok(promise.into())
        }
    }
}

fn cancel(r: &Shared, reason: JsValue, cx: &mut Context) -> JsResult<JsValue> {
    let source = r.borrow().source.clone();
    r.borrow_mut().queue.clear();
    close(r, cx);
    if let Some(source) = source
        && let Some(f) = source.get(js_string!("cancel"), cx)?.as_callable()
    {
        let v = f.call(&source.clone().into(), &[reason], cx)?;
        return Ok(JsPromise::resolve(v, cx)?.then(None, None, cx)?.into());
    }
    resolved(JsValue::undefined(), cx)
}

/// A new stream with no source (Rust or a transform feeds it): the object
/// and its state.
pub(crate) fn readable(cx: &mut Context) -> JsResult<(JsObject, Shared)> {
    let shared: Shared = Gc::new(GcRefCell::new(Readable::default()));
    let o = ReadableStream::from_data(ReadableStream { inner: shared.clone() }, cx)?;
    Ok((o, shared))
}

fn shared_of(this: &JsValue) -> JsResult<Shared> {
    this.as_object()
        .and_then(|o| o.downcast_ref::<ReadableStream>().map(|s| s.inner.clone()))
        .ok_or_else(|| type_error("not a ReadableStream"))
}

/// A native function closing over `captures`.
fn native<T: Trace + 'static>(cx: &mut Context, captures: T, f: fn(&JsValue, &[JsValue], &T, &mut Context) -> JsResult<JsValue>) -> JsFunction {
    FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_copy_closure_with_captures(f, captures)).build()
}

fn method(class: &mut ClassBuilder<'_>, name: &str, len: usize, f: fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>) {
    class.method(JsString::from(name), len, NativeFunction::from_fn_ptr(f));
}

fn getter(class: &mut ClassBuilder<'_>, name: &str, f: fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>) {
    let realm = class.context().realm().clone();
    let get = FunctionObjectBuilder::new(&realm, NativeFunction::from_fn_ptr(f)).name(JsString::from(format!("get {name}"))).build();
    class.accessor(JsString::from(name), Some(get), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
}

fn arg(a: &[JsValue], i: usize) -> JsValue {
    a.get(i).cloned().unwrap_or_default()
}

/// `then` on whatever `v` is (a value counts as fulfilled).
fn when(v: JsValue, on_ok: JsFunction, on_err: Option<JsFunction>, cx: &mut Context) -> JsResult<()> {
    JsPromise::resolve(v, cx)?.then(Some(on_ok), on_err, cx)?;
    Ok(())
}

// ---- ReadableStream --------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
pub(crate) struct ReadableStream {
    inner: Shared,
}

#[derive(Trace, Finalize, JsData)]
struct Controller {
    inner: Shared,
}

#[derive(Trace, Finalize, JsData)]
struct Reader {
    inner: Option<Shared>,
}

fn reader_shared(this: &JsValue) -> JsResult<Shared> {
    this.as_object()
        .and_then(|o| o.downcast_ref::<Reader>().and_then(|r| r.inner.clone()))
        .ok_or_else(|| type_error("the reader has been released"))
}

/// A reader of `r` (locking it).
fn get_reader(r: &Shared, cx: &mut Context) -> JsResult<JsObject> {
    if std::mem::replace(&mut r.borrow_mut().locked, true) {
        return Err(type_error("ReadableStream is locked"));
    }
    Reader::from_data(Reader { inner: Some(r.clone()) }, cx)
}

impl Class for Reader {
    const NAME: &'static str = "ReadableStreamDefaultReader";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        method(class, "read", 0, |this, _, cx| match reader_shared(this) {
            Ok(r) => read(&r, cx),
            Err(e) => {
                let e = e.into_opaque(cx)?;
                rejected(e, cx)
            }
        });
        method(class, "releaseLock", 0, |this, _, _| {
            if let Some(o) = this.as_object()
                && let Some(mut r) = o.downcast_mut::<Reader>()
                && let Some(s) = r.inner.take()
            {
                s.borrow_mut().locked = false;
            }
            Ok(JsValue::undefined())
        });
        method(class, "cancel", 1, |this, a, cx| {
            let r = reader_shared(this)?;
            cancel(&r, arg(a, 0), cx)
        });
        getter(class, "closed", |this, _, cx| {
            let r = reader_shared(this)?;
            let (state, error) = (r.borrow().state, r.borrow().error.clone());
            match state {
                State::Closed => resolved(JsValue::undefined(), cx),
                State::Errored => rejected(error, cx),
                State::Readable => {
                    let (promise, f) = JsPromise::new_pending(cx);
                    r.borrow_mut().closed.push(Waiter { resolve: f.resolve, reject: f.reject });
                    Ok(promise.into())
                }
            }
        });
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<Self> {
        let r = shared_of(&arg(a, 0))?;
        if std::mem::replace(&mut r.borrow_mut().locked, true) {
            return Err(type_error("ReadableStream is locked"));
        }
        Ok(Reader { inner: Some(r) })
    }
}

impl Class for Controller {
    const NAME: &'static str = "ReadableStreamDefaultController";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        let with = |this: &JsValue| -> JsResult<Shared> {
            this.as_object().and_then(|o| o.downcast_ref::<Controller>().map(|c| c.inner.clone())).ok_or_else(|| type_error("not a controller"))
        };
        let _ = with;
        method(class, "enqueue", 1, |this, a, cx| {
            let r = this.as_object().and_then(|o| o.downcast_ref::<Controller>().map(|c| c.inner.clone())).ok_or_else(|| type_error("not a controller"))?;
            if r.borrow().state != State::Readable {
                return Err(type_error("The stream is closed"));
            }
            push(&r, arg(a, 0), cx);
            Ok(JsValue::undefined())
        });
        method(class, "close", 0, |this, _, cx| {
            let r = this.as_object().and_then(|o| o.downcast_ref::<Controller>().map(|c| c.inner.clone())).ok_or_else(|| type_error("not a controller"))?;
            close(&r, cx);
            Ok(JsValue::undefined())
        });
        method(class, "error", 1, |this, a, cx| {
            let r = this.as_object().and_then(|o| o.downcast_ref::<Controller>().map(|c| c.inner.clone())).ok_or_else(|| type_error("not a controller"))?;
            fail(&r, arg(a, 0), cx);
            Ok(JsValue::undefined())
        });
        getter(class, "desiredSize", |this, _, _| {
            let r = this.as_object().and_then(|o| o.downcast_ref::<Controller>().map(|c| c.inner.clone())).ok_or_else(|| type_error("not a controller"))?;
            let s = r.borrow();
            Ok(match s.state {
                State::Readable => JsValue::from(1 - s.queue.len() as i32),
                State::Closed => JsValue::from(0),
                State::Errored => JsValue::null(),
            })
        });
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(type_error("Illegal constructor"))
    }
}

/// `pipeTo`: read `r`, write each chunk to `writer` (after the last one
/// settles), then close it; the promise settles with the copy.
#[derive(Trace, Finalize, Clone)]
struct Pipe {
    from: Shared,
    writer: JsObject,
    resolve: JsFunction,
    reject: JsFunction,
}

fn pipe_step(p: &Pipe, cx: &mut Context) -> JsResult<()> {
    let next = read(&p.from, cx)?;
    let on_read = native(cx, p.clone(), |_, a, p, cx| {
        let result = arg(a, 0);
        let o = result.as_object().ok_or_else(|| type_error("bad read"))?;
        let done = o.get(js_string!("done"), cx)?.to_boolean();
        if done {
            let close = p.writer.get(js_string!("close"), cx)?;
            let closed = close.as_callable().map(|f| f.call(&p.writer.clone().into(), &[], cx)).transpose()?.unwrap_or_default();
            let finish = native(cx, p.clone(), |_, _, p, cx| p.resolve.call(&JsValue::undefined(), &[], cx));
            let failed = native(cx, p.clone(), |_, a, p, cx| p.reject.call(&JsValue::undefined(), &[arg(a, 0)], cx));
            when(closed, finish, Some(failed), cx)?;
            return Ok(JsValue::undefined());
        }
        let value = o.get(js_string!("value"), cx)?;
        let write = p.writer.get(js_string!("write"), cx)?;
        let written = write.as_callable().ok_or_else(|| type_error("no write"))?.call(&p.writer.clone().into(), &[value], cx)?;
        let again = native(cx, p.clone(), |_, _, p, cx| {
            pipe_step(p, cx)?;
            Ok(JsValue::undefined())
        });
        let failed = native(cx, p.clone(), |_, a, p, cx| p.reject.call(&JsValue::undefined(), &[arg(a, 0)], cx));
        when(written, again, Some(failed), cx)?;
        Ok(JsValue::undefined())
    });
    let failed = native(cx, p.clone(), |_, a, p, cx| {
        let abort = p.writer.get(js_string!("abort"), cx)?;
        if let Some(abort) = abort.as_callable() {
            abort.call(&p.writer.clone().into(), &[arg(a, 0)], cx).ok();
        }
        p.reject.call(&JsValue::undefined(), &[arg(a, 0)], cx)
    });
    when(next, on_read, Some(failed), cx)
}

fn pipe_to(r: &Shared, writable: &JsValue, cx: &mut Context) -> JsResult<JsValue> {
    let (promise, f) = JsPromise::new_pending(cx);
    let started = (|| -> JsResult<Pipe> {
        let w = writable.as_object().ok_or_else(|| type_error("pipeTo needs a WritableStream"))?;
        let get_writer = w.get(js_string!("getWriter"), cx)?;
        let writer = get_writer.as_callable().ok_or_else(|| type_error("pipeTo needs a WritableStream"))?.call(&w.into(), &[], cx)?;
        let writer = writer.as_object().ok_or_else(|| type_error("no writer"))?;
        if std::mem::replace(&mut r.borrow_mut().locked, true) {
            return Err(type_error("ReadableStream is locked"));
        }
        Ok(Pipe { from: r.clone(), writer, resolve: f.resolve.clone(), reject: f.reject.clone() })
    })();
    match started {
        Ok(p) => pipe_step(&p, cx)?,
        Err(e) => {
            let e = e.into_opaque(cx)?;
            f.reject.call(&JsValue::undefined(), &[e], cx)?;
        }
    }
    Ok(promise.into())
}

/// `tee`: two streams that each get every chunk.
#[derive(Trace, Finalize, Clone)]
struct Tee {
    from: Shared,
    a: Shared,
    b: Shared,
}

fn tee_step(t: &Tee, cx: &mut Context) -> JsResult<()> {
    let next = read(&t.from, cx)?;
    let on_read = native(cx, t.clone(), |_, a, t, cx| {
        let o = arg(a, 0).as_object().ok_or_else(|| type_error("bad read"))?;
        if o.get(js_string!("done"), cx)?.to_boolean() {
            close(&t.a, cx);
            close(&t.b, cx);
            return Ok(JsValue::undefined());
        }
        let value = o.get(js_string!("value"), cx)?;
        push(&t.a, value.clone(), cx);
        push(&t.b, value, cx);
        tee_step(t, cx)?;
        Ok(JsValue::undefined())
    });
    let failed = native(cx, t.clone(), |_, a, t, cx| {
        fail(&t.a, arg(a, 0), cx);
        fail(&t.b, arg(a, 0), cx);
        Ok(JsValue::undefined())
    });
    when(next, on_read, Some(failed), cx)
}

/// `ReadableStream.from(iterable)`: its values (awaited), then closed.
#[derive(Trace, Finalize, Clone)]
struct Pump {
    iterator: JsObject,
    next: JsObject,
    to: Shared,
}

fn pump(p: &Pump, cx: &mut Context) -> JsResult<()> {
    let result = p.next.call(&p.iterator.clone().into(), &[], cx)?;
    let on_result = native(cx, p.clone(), |_, a, p, cx| {
        let o = arg(a, 0).as_object().ok_or_else(|| type_error("iterator result is not an object"))?;
        if o.get(js_string!("done"), cx)?.to_boolean() {
            close(&p.to, cx);
            return Ok(JsValue::undefined());
        }
        let value = o.get(js_string!("value"), cx)?;
        let on_value = native(cx, p.clone(), |_, a, p, cx| {
            push(&p.to, arg(a, 0), cx);
            pump(p, cx)?;
            Ok(JsValue::undefined())
        });
        let failed = native(cx, p.to.clone(), |_, a, to, cx| {
            fail(to, arg(a, 0), cx);
            Ok(JsValue::undefined())
        });
        when(value, on_value, Some(failed), cx)?;
        Ok(JsValue::undefined())
    });
    let failed = native(cx, p.to.clone(), |_, a, to, cx| {
        fail(to, arg(a, 0), cx);
        Ok(JsValue::undefined())
    });
    when(result, on_result, Some(failed), cx)
}

/// An async iterator over `r` (`for await (const chunk of stream)`).
fn iterate(r: &Shared, cx: &mut Context) -> JsResult<JsValue> {
    let reader = get_reader(r, cx)?;
    let it = JsObject::with_object_proto(cx.intrinsics());
    let next = native(cx, reader.clone(), |_, _, reader, cx| {
        let r = reader_shared(&reader.clone().into())?;
        read(&r, cx)
    });
    let ret = native(cx, reader.clone(), |_, a, reader, cx| {
        if let Ok(r) = reader_shared(&reader.clone().into()) {
            cancel(&r, arg(a, 0), cx)?;
            r.borrow_mut().locked = false;
        }
        let v = step(JsValue::undefined(), true, cx);
        resolved(v, cx)
    });
    it.set(js_string!("next"), next, false, cx)?;
    it.set(js_string!("return"), ret, false, cx)?;
    let me = native(cx, it.clone(), |_, _, it, _| Ok(it.clone().into()));
    it.set(JsSymbol::async_iterator(), me, false, cx)?;
    Ok(it.into())
}

impl Class for ReadableStream {
    const NAME: &'static str = "ReadableStream";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        getter(class, "locked", |this, _, _| Ok(shared_of(this)?.borrow().locked.into()));
        method(class, "getReader", 0, |this, _, cx| Ok(get_reader(&shared_of(this)?, cx)?.into()));
        method(class, "cancel", 1, |this, a, cx| {
            let r = shared_of(this)?;
            if r.borrow().locked {
                return rejected(JsNativeError::typ().with_message("ReadableStream is locked").into_opaque(cx).into(), cx);
            }
            cancel(&r, arg(a, 0), cx)
        });
        method(class, "pipeTo", 1, |this, a, cx| pipe_to(&shared_of(this)?, &arg(a, 0), cx));
        method(class, "pipeThrough", 1, |this, a, cx| {
            let r = shared_of(this)?;
            let t = arg(a, 0).as_object().ok_or_else(|| type_error("pipeThrough needs a transform"))?;
            let (writable, readable) = (t.get(js_string!("writable"), cx)?, t.get(js_string!("readable"), cx)?);
            let piped = pipe_to(&r, &writable, cx)?;
            // Its failures end up at the readable side.
            if let Ok(rs) = shared_of(&readable) {
                let failed = native(cx, rs, |_, a, rs, cx| {
                    fail(rs, arg(a, 0), cx);
                    Ok(JsValue::undefined())
                });
                if let Some(p) = piped.as_object() {
                    JsPromise::from_object(p)?.then(None, Some(failed), cx)?;
                }
            }
            Ok(readable)
        });
        method(class, "tee", 0, |this, _, cx| {
            let r = shared_of(this)?;
            if std::mem::replace(&mut r.borrow_mut().locked, true) {
                return Err(type_error("ReadableStream is locked"));
            }
            let (a, sa) = readable(cx)?;
            let (b, sb) = readable(cx)?;
            tee_step(&Tee { from: r, a: sa, b: sb }, cx)?;
            Ok(boa_engine::object::builtins::JsArray::from_iter([a.into(), b.into()], cx).into())
        });
        method(class, "values", 0, |this, _, cx| iterate(&shared_of(this)?, cx));
        let realm = class.context().realm().clone();
        let iter = FunctionObjectBuilder::new(&realm, NativeFunction::from_fn_ptr(|this, _, cx| iterate(&shared_of(this)?, cx))).name(js_string!("[Symbol.asyncIterator]")).build();
        class.property(JsSymbol::async_iterator(), iter, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        // For the prelude's Response, which feeds its body stream.
        method(class, "_push", 1, |this, a, cx| {
            push(&shared_of(this)?, arg(a, 0), cx);
            Ok(JsValue::undefined())
        });
        method(class, "_close", 0, |this, _, cx| {
            close(&shared_of(this)?, cx);
            Ok(JsValue::undefined())
        });
        method(class, "_fail", 1, |this, a, cx| {
            fail(&shared_of(this)?, arg(a, 0), cx);
            Ok(JsValue::undefined())
        });
        class.static_method(js_string!("from"), 1, NativeFunction::from_fn_ptr(|_, a, cx| {
            let iterable = arg(a, 0);
            let o = iterable.as_object().ok_or_else(|| type_error("ReadableStream.from needs an iterable"))?;
            let method = match o.get(JsSymbol::async_iterator(), cx)? {
                m if m.is_callable() => m,
                _ => o.get(JsSymbol::iterator(), cx)?,
            };
            let method = method.as_callable().ok_or_else(|| type_error("ReadableStream.from needs an iterable"))?.clone();
            let iterator = method.call(&iterable, &[], cx)?.as_object().ok_or_else(|| type_error("not an iterator"))?;
            let next = iterator.get(js_string!("next"), cx)?.as_object().filter(|n| n.is_callable()).ok_or_else(|| type_error("not an iterator"))?;
            let (stream, shared) = readable(cx)?;
            pump(&Pump { iterator, next, to: shared }, cx)?;
            Ok(stream.into())
        }));
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Ok(ReadableStream { inner: Gc::new(GcRefCell::new(Readable::default())) })
    }

    fn object_constructor(instance: &JsObject<Self>, a: &[JsValue], cx: &mut Context) -> JsResult<()> {
        let inner = instance.borrow().data().inner.clone();
        let Some(source) = a.first().and_then(JsValue::as_object) else { return Ok(()) };
        let controller = Controller::from_data(Controller { inner: inner.clone() }, cx)?;
        {
            let mut s = inner.borrow_mut();
            s.source = Some(source.clone());
            s.controller = Some(controller.clone());
        }
        let start = source.get(js_string!("start"), cx)?;
        if let Some(start) = start.as_callable() {
            match start.call(&source.clone().into(), &[controller.into()], cx) {
                Err(e) => {
                    let e = e.into_opaque(cx)?;
                    fail(&inner, e, cx);
                }
                // A source that pulls: once started, it's asked for a chunk.
                Ok(v) => {
                    let begin = native(cx, inner.clone(), |_, _, r, cx| {
                        pull(r, cx);
                        Ok(JsValue::undefined())
                    });
                    let failed = native(cx, inner.clone(), |_, a, r, cx| {
                        fail(r, arg(a, 0), cx);
                        Ok(JsValue::undefined())
                    });
                    when(v, begin, Some(failed), cx)?;
                }
            }
        } else {
            pull(&inner, cx);
        }
        Ok(())
    }
}

// ---- WritableStream ----------------------------------------------------------

/// Where a writable's chunks go.
#[derive(Trace, Finalize, Clone)]
enum Sink {
    /// A script's underlying sink (`write`, `close`, `abort`).
    Js(Option<JsObject>),
    /// A transform's writable side.
    Transform(Gc<Transform>),
}

#[derive(Trace, Finalize, JsData)]
struct WritableStream {
    sink: Sink,
    #[unsafe_ignore_trace]
    locked: std::cell::Cell<bool>,
}

#[derive(Trace, Finalize, JsData)]
struct Writer {
    sink: Sink,
    /// The last write (or close), which the next waits for.
    last: GcRefCell<JsValue>,
    #[unsafe_ignore_trace]
    closed: std::cell::Cell<bool>,
}

/// `sink.<name>(chunk)` as a promise.
fn sink_call(sink: &Sink, name: &str, chunk: JsValue, cx: &mut Context) -> JsResult<JsValue> {
    match sink {
        Sink::Js(None) => resolved(JsValue::undefined(), cx),
        Sink::Js(Some(o)) => {
            let f = o.get(JsString::from(name), cx)?;
            let v = match f.as_callable() {
                Some(f) => f.call(&o.clone().into(), &[chunk], cx)?,
                None => JsValue::undefined(),
            };
            resolved(v, cx)
        }
        Sink::Transform(t) => match name {
            "write" => transform_write(t, chunk, cx),
            "close" => transform_close(t, cx),
            _ => {
                fail(&t.readable, chunk, cx);
                resolved(JsValue::undefined(), cx)
            }
        },
    }
}

fn writer_of(this: &JsValue) -> JsResult<JsObject> {
    this.as_object().filter(|o| o.downcast_ref::<Writer>().is_some()).ok_or_else(|| type_error("not a WritableStreamDefaultWriter"))
}

/// Run `name` with `chunk` after the writer's last operation settles.
fn queued(this: &JsValue, name: &'static str, chunk: JsValue, cx: &mut Context) -> JsResult<JsValue> {
    let w = writer_of(this)?;
    let (sink, last) = {
        let wr = w.downcast_ref::<Writer>().expect("checked");
        if wr.closed.get() {
            return rejected(JsNativeError::typ().with_message("The stream is closed").into_opaque(cx).into(), cx);
        }
        if name == "close" {
            wr.closed.set(true);
        }
        (wr.sink.clone(), wr.last.borrow().clone())
    };
    let next = if last.is_undefined() {
        sink_call(&sink, name, chunk, cx)?
    } else {
        #[derive(Trace, Finalize)]
        struct Then {
            sink: Sink,
            #[unsafe_ignore_trace]
            name: &'static str,
            chunk: JsValue,
        }
        let go = native(cx, Then { sink, name, chunk }, |_, _, t, cx| sink_call(&t.sink, t.name, t.chunk.clone(), cx));
        let p = JsPromise::resolve(last, cx)?;
        p.then(Some(go), None, cx)?.into()
    };
    *w.downcast_ref::<Writer>().expect("checked").last.borrow_mut() = next.clone();
    Ok(next)
}

impl Class for Writer {
    const NAME: &'static str = "WritableStreamDefaultWriter";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        method(class, "write", 1, |this, a, cx| queued(this, "write", arg(a, 0), cx));
        method(class, "close", 0, |this, _, cx| queued(this, "close", JsValue::undefined(), cx));
        method(class, "abort", 1, |this, a, cx| queued(this, "abort", arg(a, 0), cx));
        method(class, "releaseLock", 0, |_, _, _| Ok(JsValue::undefined()));
        getter(class, "ready", |_, _, cx| resolved(JsValue::undefined(), cx));
        getter(class, "closed", |this, _, cx| {
            let last = writer_of(this)?.downcast_ref::<Writer>().expect("checked").last.borrow().clone();
            Ok(JsPromise::resolve(last, cx)?.then(None, None, cx)?.into())
        });
        getter(class, "desiredSize", |_, _, _| Ok(JsValue::from(1)));
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<Self> {
        let o = arg(a, 0).as_object().ok_or_else(|| type_error("not a WritableStream"))?;
        let w = o.downcast_ref::<WritableStream>().ok_or_else(|| type_error("not a WritableStream"))?;
        Ok(Writer { sink: w.sink.clone(), last: GcRefCell::new(JsValue::undefined()), closed: std::cell::Cell::new(false) })
    }
}

fn writable_of(this: &JsValue) -> JsResult<JsObject> {
    this.as_object().filter(|o| o.downcast_ref::<WritableStream>().is_some()).ok_or_else(|| type_error("not a WritableStream"))
}

fn get_writer(this: &JsValue, cx: &mut Context) -> JsResult<JsObject> {
    let o = writable_of(this)?;
    let sink = {
        let w = o.downcast_ref::<WritableStream>().expect("checked");
        if w.locked.replace(true) {
            return Err(type_error("WritableStream is locked"));
        }
        w.sink.clone()
    };
    Writer::from_data(Writer { sink, last: GcRefCell::new(JsValue::undefined()), closed: std::cell::Cell::new(false) }, cx)
}

impl Class for WritableStream {
    const NAME: &'static str = "WritableStream";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        method(class, "getWriter", 0, |this, _, cx| Ok(get_writer(this, cx)?.into()));
        getter(class, "locked", |this, _, _| Ok(writable_of(this)?.downcast_ref::<WritableStream>().expect("checked").locked.get().into()));
        method(class, "close", 0, |this, _, cx| {
            let w = get_writer(this, cx)?;
            queued(&w.into(), "close", JsValue::undefined(), cx)
        });
        method(class, "abort", 1, |this, a, cx| {
            let w = get_writer(this, cx)?;
            queued(&w.into(), "abort", arg(a, 0), cx)
        });
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let sink = a.first().and_then(JsValue::as_object);
        if let Some(sink) = &sink
            && let Some(start) = sink.get(js_string!("start"), cx)?.as_callable()
        {
            let controller = JsObject::with_object_proto(cx.intrinsics());
            start.call(&sink.clone().into(), &[controller.into()], cx)?;
        }
        Ok(WritableStream { sink: Sink::Js(sink), locked: std::cell::Cell::new(false) })
    }
}

// ---- TransformStream -----------------------------------------------------------

/// What a transform does with each chunk.
#[derive(Trace, Finalize)]
enum Transformer {
    /// A script's (`transform`, `flush`) and the controller it's given.
    Js { transformer: Option<JsObject>, controller: JsObject },
    /// Bytes to text (`TextDecoderStream`).
    Decode(#[unsafe_ignore_trace] GcRefCell<encoding_rs::Decoder>),
    /// Text to UTF-8 bytes (`TextEncoderStream`).
    Encode,
}

#[derive(Trace, Finalize)]
struct Transform {
    readable: Shared,
    transformer: Transformer,
}

fn transform_write(t: &Gc<Transform>, chunk: JsValue, cx: &mut Context) -> JsResult<JsValue> {
    match &t.transformer {
        Transformer::Js { transformer, controller } => {
            let f = transformer.as_ref().map(|o| o.get(js_string!("transform"), cx)).transpose()?.filter(JsValue::is_callable);
            match (f, transformer) {
                (Some(f), Some(o)) => {
                    let v = f.as_callable().expect("callable").call(&o.clone().into(), &[chunk, controller.clone().into()], cx)?;
                    resolved(v, cx)
                }
                _ => {
                    push(&t.readable, chunk, cx);
                    resolved(JsValue::undefined(), cx)
                }
            }
        }
        Transformer::Decode(decoder) => {
            let bytes = crate::blob::binary_of(&chunk, cx)?.ok_or_else(|| type_error("TextDecoderStream: chunks must be BufferSource"))?;
            let mut text = String::with_capacity(bytes.len() + 4);
            let mut d = decoder.borrow_mut();
            text.reserve(d.max_utf8_buffer_length(bytes.len()).unwrap_or(bytes.len() * 3));
            let _ = d.decode_to_string(&bytes, &mut text, false);
            drop(d);
            if !text.is_empty() {
                push(&t.readable, JsString::from(text).into(), cx);
            }
            resolved(JsValue::undefined(), cx)
        }
        Transformer::Encode => {
            let text = chunk.to_string(cx)?.to_std_string_escaped();
            if !text.is_empty() {
                let bytes = crate::blob::uint8_array(text.as_bytes(), cx)?;
                push(&t.readable, bytes, cx);
            }
            resolved(JsValue::undefined(), cx)
        }
    }
}

fn transform_close(t: &Gc<Transform>, cx: &mut Context) -> JsResult<JsValue> {
    match &t.transformer {
        Transformer::Js { transformer: Some(o), controller } => {
            if let Some(flush) = o.get(js_string!("flush"), cx)?.as_callable() {
                let v = flush.call(&o.clone().into(), &[controller.clone().into()], cx)?;
                let done = native(cx, t.readable.clone(), |_, _, r, cx| {
                    close(r, cx);
                    Ok(JsValue::undefined())
                });
                JsPromise::resolve(v, cx)?.then(Some(done), None, cx)?;
                return resolved(JsValue::undefined(), cx);
            }
        }
        Transformer::Decode(decoder) => {
            let mut text = String::new();
            let mut d = decoder.borrow_mut();
            text.reserve(d.max_utf8_buffer_length(0).unwrap_or(4));
            let _ = d.decode_to_string(&[], &mut text, true);
            drop(d);
            if !text.is_empty() {
                push(&t.readable, JsString::from(text).into(), cx);
            }
        }
        _ => {}
    }
    close(&t.readable, cx);
    resolved(JsValue::undefined(), cx)
}

#[derive(Trace, Finalize, JsData)]
struct TransformController {
    readable: Shared,
}

impl Class for TransformController {
    const NAME: &'static str = "TransformStreamDefaultController";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        fn of(this: &JsValue) -> JsResult<Shared> {
            this.as_object().and_then(|o| o.downcast_ref::<TransformController>().map(|c| c.readable.clone())).ok_or_else(|| type_error("not a controller"))
        }
        method(class, "enqueue", 1, |this, a, cx| {
            push(&of(this)?, arg(a, 0), cx);
            Ok(JsValue::undefined())
        });
        method(class, "error", 1, |this, a, cx| {
            fail(&of(this)?, arg(a, 0), cx);
            Ok(JsValue::undefined())
        });
        method(class, "terminate", 0, |this, _, cx| {
            close(&of(this)?, cx);
            Ok(JsValue::undefined())
        });
        getter(class, "desiredSize", |this, _, _| Ok(JsValue::from(1 - of(this)?.borrow().queue.len() as i32)));
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(type_error("Illegal constructor"))
    }
}

/// A transform's two sides (and the encoding, for the text ones).
#[derive(Trace, Finalize)]
struct Pair {
    readable: JsObject,
    writable: JsObject,
    #[unsafe_ignore_trace]
    encoding: &'static str,
}

fn make_pair(transformer: impl FnOnce(&Shared, &mut Context) -> JsResult<Transformer>, encoding: &'static str, cx: &mut Context) -> JsResult<Pair> {
    let (readable, shared) = readable(cx)?;
    let transformer = transformer(&shared, cx)?;
    let t = Gc::new(Transform { readable: shared, transformer });
    let writable = WritableStream::from_data(WritableStream { sink: Sink::Transform(t), locked: std::cell::Cell::new(false) }, cx)?;
    Ok(Pair { readable, writable, encoding })
}

/// The transform classes: one shape (`readable`, `writable`), three kinds.
trait TransformKind: 'static {
    const NAME: &'static str;
    fn make(a: &[JsValue], cx: &mut Context) -> JsResult<Pair>;
}

#[derive(Trace, Finalize, JsData)]
struct TransformClass<K: TransformKind> {
    pair: Pair,
    #[unsafe_ignore_trace]
    kind: std::marker::PhantomData<K>,
}

fn pair_get<K: TransformKind>(this: &JsValue, f: fn(&Pair) -> JsValue) -> JsResult<JsValue> {
    let o = this.as_object().ok_or_else(|| type_error("not a transform stream"))?;
    let t = o.downcast_ref::<TransformClass<K>>().ok_or_else(|| type_error("not a transform stream"))?;
    Ok(f(&t.pair))
}

impl<K: TransformKind> Class for TransformClass<K> {
    const NAME: &'static str = K::NAME;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        getter(class, "readable", |this, _, _| pair_get::<K>(this, |p| p.readable.clone().into()));
        getter(class, "writable", |this, _, _| pair_get::<K>(this, |p| p.writable.clone().into()));
        if K::NAME != "TransformStream" {
            getter(class, "encoding", |this, _, _| pair_get::<K>(this, |p| JsString::from(p.encoding).into()));
        }
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        Ok(TransformClass { pair: K::make(a, cx)?, kind: std::marker::PhantomData })
    }
}

struct Plain;
impl TransformKind for Plain {
    const NAME: &'static str = "TransformStream";
    fn make(a: &[JsValue], cx: &mut Context) -> JsResult<Pair> {
        let transformer = a.first().and_then(JsValue::as_object);
        make_pair(
            |shared, cx| {
                let controller = TransformController::from_data(TransformController { readable: shared.clone() }, cx)?;
                if let Some(t) = &transformer
                    && let Some(start) = t.get(js_string!("start"), cx)?.as_callable()
                {
                    start.call(&t.clone().into(), &[controller.clone().into()], cx)?;
                }
                Ok(Transformer::Js { transformer, controller })
            },
            "",
            cx,
        )
    }
}

struct Decoding;
impl TransformKind for Decoding {
    const NAME: &'static str = "TextDecoderStream";
    fn make(a: &[JsValue], cx: &mut Context) -> JsResult<Pair> {
        let label = match a.first() {
            Some(l) if !l.is_undefined() => l.to_string(cx)?.to_std_string_escaped(),
            _ => "utf-8".into(),
        };
        let encoding = encoding_rs::Encoding::for_label(label.trim().as_bytes()).ok_or_else(|| JsNativeError::range().with_message(format!("TextDecoderStream: unknown encoding {label:?}")))?;
        let name: &'static str = Box::leak(encoding.name().to_ascii_lowercase().into_boxed_str());
        make_pair(|_, _| Ok(Transformer::Decode(GcRefCell::new(encoding.new_decoder_with_bom_removal()))), name, cx)
    }
}

struct Encoding;
impl TransformKind for Encoding {
    const NAME: &'static str = "TextEncoderStream";
    fn make(_: &[JsValue], cx: &mut Context) -> JsResult<Pair> {
        make_pair(|_, _| Ok(Transformer::Encode), "utf-8", cx)
    }
}

/// Register the stream classes.
pub(crate) fn install(cx: &mut Context) {
    let results = [
        cx.register_global_class::<ReadableStream>(),
        cx.register_global_class::<Reader>(),
        cx.register_global_class::<Controller>(),
        cx.register_global_class::<WritableStream>(),
        cx.register_global_class::<Writer>(),
        cx.register_global_class::<TransformController>(),
        cx.register_global_class::<TransformClass<Plain>>(),
        cx.register_global_class::<TransformClass<Decoding>>(),
        cx.register_global_class::<TransformClass<Encoding>>(),
    ];
    for r in results {
        if let Err(e) = r {
            eprintln!("js: streams: {e}");
        }
    }
}
