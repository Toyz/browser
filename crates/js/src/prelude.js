// Feather's DOM, in JavaScript, over `__native` (node ids in, node ids out).
// Wrappers, listeners and timers live here, in the JS heap.
(function (global) {
  "use strict";

  // Web-legacy (Annex B) bits real pages still use that Boa leaves out.
  const legacy = (obj, name, value) => {
    if (!(name in obj)) Object.defineProperty(obj, name, { value, writable: true, configurable: true });
  };
  // Intl.DateTimeFormat on Temporal (Boa's own ignores most options and
  // can't make parts): US English, any IANA time zone, the system's by default.
  if (typeof Intl !== "undefined" && typeof Temporal !== "undefined") {
    const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    const DAYS = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
    const STYLES = {
      full: { weekday: "long", year: "numeric", month: "long", day: "numeric" },
      long: { year: "numeric", month: "long", day: "numeric" },
      medium: { year: "numeric", month: "short", day: "numeric" },
      short: { year: "2-digit", month: "numeric", day: "numeric" },
    };
    const TIME_STYLES = {
      full: { hour: "numeric", minute: "2-digit", second: "2-digit", timeZoneName: "long" },
      long: { hour: "numeric", minute: "2-digit", second: "2-digit", timeZoneName: "short" },
      medium: { hour: "numeric", minute: "2-digit", second: "2-digit" },
      short: { hour: "numeric", minute: "2-digit" },
    };
    const COMPONENTS = ["weekday", "era", "year", "month", "day", "hour", "minute", "second", "fractionalSecondDigits", "timeZoneName", "dayPeriod"];
    // en-US names US zones by abbreviation (EST/EDT...), everything else GMT±n.
    // [prefix, standard offset, long name]
    function usZone(tz) {
      if (/^(America\/(New_York|Detroit|Indiana|Kentucky|Toronto|Nassau)|US\/Eastern|EST5EDT)/.test(tz)) return ["E", -5, "Eastern"];
      if (/^(America\/(Chicago|Winnipeg|Menominee|North_Dakota|Indiana\/(Knox|Tell_City))|US\/Central|CST6CDT)/.test(tz)) return ["C", -6, "Central"];
      if (/^(America\/(Denver|Boise|Edmonton|Phoenix|Ojinaga)|US\/(Mountain|Arizona)|MST7MDT)/.test(tz)) return ["M", -7, "Mountain"];
      if (/^(America\/(Los_Angeles|Vancouver|Tijuana)|US\/Pacific|PST8PDT)/.test(tz)) return ["P", -8, "Pacific"];
      if (/^(America\/(Anchorage|Juneau|Sitka|Nome|Yakutat)|US\/Alaska)/.test(tz)) return ["AK", -9, "Alaska"];
      if (/^(Pacific\/Honolulu|US\/Hawaii)/.test(tz)) return ["H", -10, "Hawaii-Aleutian"];
      return null;
    }
    function DateTimeFormat(locales, options) {
      if (!(this instanceof DateTimeFormat)) return new DateTimeFormat(locales, options);
      const o = Object.assign({}, options || {});
      if (o.dateStyle) Object.assign(o, STYLES[o.dateStyle]);
      if (o.timeStyle) Object.assign(o, TIME_STYLES[o.timeStyle]);
      if (!COMPONENTS.some((k) => o[k] !== undefined)) Object.assign(o, { year: "numeric", month: "numeric", day: "numeric" });
      if (o.hour12 === undefined) o.hour12 = o.hourCycle ? /h1[12]/.test(o.hourCycle) : true;
      this._o = o;
      this._tz = o.timeZone || N.timeZone();
    }
    const num = (v, style) => style === "2-digit" ? String(v % 100).padStart(2, "0") : String(v);
    DateTimeFormat.prototype.formatToParts = function (date) {
      const o = this._o;
      const ms = date === undefined ? Date.now() : +date;
      if (isNaN(ms)) throw new RangeError("Invalid time value");
      const z = Temporal.Instant.fromEpochMilliseconds(Math.trunc(ms)).toZonedDateTimeISO(this._tz);
      const parts = [];
      const lit = (v) => parts.push({ type: "literal", value: v });
      const textMonth = o.month === "long" || o.month === "short" || o.month === "narrow";
      if (o.weekday) {
        const w = DAYS[z.dayOfWeek - 1];
        parts.push({ type: "weekday", value: o.weekday === "long" ? w : o.weekday === "short" ? w.slice(0, 3) : w[0] });
        if (o.year || o.month || o.day) lit(", ");
      }
      if (textMonth) {
        const m = MONTHS[z.month - 1];
        parts.push({ type: "month", value: o.month === "long" ? m : o.month === "short" ? m.slice(0, 3) : m[0] });
        if (o.day) { lit(" "); parts.push({ type: "day", value: num(z.day, o.day) }); }
        if (o.year) { lit(o.day ? ", " : " "); parts.push({ type: "year", value: num(z.year, o.year) }); }
      } else {
        const date = [];
        if (o.month) date.push({ type: "month", value: num(z.month, o.month) });
        if (o.day) date.push({ type: "day", value: num(z.day, o.day) });
        if (o.year) date.push({ type: "year", value: num(z.year, o.year) });
        date.forEach((p, i) => { if (i) lit("/"); parts.push(p); });
      }
      if (o.hour || o.minute || o.second) {
        if (parts.length) lit(", ");
        const h12 = o.hour12;
        if (o.hour) parts.push({ type: "hour", value: h12 ? num((z.hour % 12) || 12, o.hour) : num(z.hour, o.hour === "numeric" && !h12 ? "2-digit" : o.hour) });
        if (o.minute) { if (o.hour) lit(":"); parts.push({ type: "minute", value: String(z.minute).padStart(2, "0") }); }
        if (o.second) { if (o.hour || o.minute) lit(":"); parts.push({ type: "second", value: String(z.second).padStart(2, "0") }); }
        if (o.hour && h12) { lit(" "); parts.push({ type: "dayPeriod", value: z.hour < 12 ? "AM" : "PM" }); }
      }
      if (o.timeZoneName) {
        lit(" ");
        let name;
        if (/^(UTC|Etc\/UTC|Etc\/GMT)$/i.test(this._tz)) name = o.timeZoneName === "long" ? "Coordinated Universal Time" : "UTC";
        else {
          const [, sign, hh, mm] = /([+-])(\d\d):(\d\d)/.exec(z.offset) || [, "+", "00", "00"];
          const hours = (sign === "-" ? -1 : 1) * (Number(hh) + Number(mm) / 60);
          const us = usZone(this._tz);
          if (us) {
            const dst = hours !== us[1];
            name = o.timeZoneName === "long" ? us[2] + (dst ? " Daylight Time" : " Standard Time") : us[0] + (dst ? "DT" : "ST");
          } else {
            name = "GMT" + (hh === "00" && mm === "00" ? "" : sign + Number(hh) + (mm !== "00" ? ":" + mm : ""));
          }
        }
        parts.push({ type: "timeZoneName", value: name });
      }
      return parts;
    };
    DateTimeFormat.prototype.format = function (date) { return this.formatToParts(date).map((p) => p.value).join(""); };
    DateTimeFormat.prototype.formatRange = function (a, b) { return this.format(a) + " – " + this.format(b); };
    DateTimeFormat.prototype.resolvedOptions = function () {
      const out = { locale: "en-US", calendar: "gregory", numberingSystem: "latn", timeZone: this._tz };
      for (const k of COMPONENTS.concat(["hour12", "dateStyle", "timeStyle"])) if (this._o[k] !== undefined) out[k] = this._o[k];
      if (this._o.hour) out.hourCycle = this._o.hour12 ? "h12" : "h23";
      return out;
    };
    DateTimeFormat.supportedLocalesOf = (l) => [].concat(l || []);
    Intl.DateTimeFormat = DateTimeFormat;
    const locale = (opts, defaults) => {
      const o = Object.assign({}, opts || {});
      if (!COMPONENTS.some((k) => o[k] !== undefined) && !o.dateStyle && !o.timeStyle) Object.assign(o, defaults);
      return o;
    };
    Date.prototype.toLocaleString = function (l, o) { return new DateTimeFormat(l, locale(o, { year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "2-digit", second: "2-digit" })).format(this); };
    Date.prototype.toLocaleDateString = function (l, o) { return new DateTimeFormat(l, locale(o, { year: "numeric", month: "numeric", day: "numeric" })).format(this); };
    Date.prototype.toLocaleTimeString = function (l, o) { return new DateTimeFormat(l, locale(o, { hour: "numeric", minute: "2-digit", second: "2-digit" })).format(this); };
  }
  if (typeof Intl !== "undefined" && Intl.NumberFormat && !Intl.NumberFormat.prototype.formatToParts) {
    legacy(Intl.NumberFormat.prototype, "formatToParts", function (n) {
      const parts = [];
      let seenDecimal = false;
      for (const piece of this.format(n).match(/\d+|[.,]|[^\d.,]+/g) || []) {
        if (/^\d+$/.test(piece)) parts.push({ type: seenDecimal ? "fraction" : "integer", value: piece });
        else if (piece === ".") { seenDecimal = true; parts.push({ type: "decimal", value: piece }); }
        else if (piece === ",") parts.push({ type: "group", value: piece });
        else parts.push({ type: /-/.test(piece) ? "minusSign" : /%/.test(piece) ? "percentSign" : "literal", value: piece });
      }
      return parts;
    });
  }
  legacy(Object.prototype, "__defineGetter__", function (k, fn) { Object.defineProperty(this, k, { get: fn, configurable: true, enumerable: true }); });
  legacy(Object.prototype, "__defineSetter__", function (k, fn) { Object.defineProperty(this, k, { set: fn, configurable: true, enumerable: true }); });
  legacy(Object.prototype, "__lookupGetter__", function (k) { const d = Object.getOwnPropertyDescriptor(this, k); return d && d.get; });
  const N = __native;
  // FEATHER_JS_TRACE: who removes big things (debugging pages that vanish).
  if (N.trace()) {
    const remove = N.remove, setText = N.setText;
    const describe = (id) => "<" + N.tag(id) + " class=" + N.getAttr(id, "class") + " id=" + N.getAttr(id, "id") + ">";
    N.remove = (id) => { if (["div", "main", "body"].includes(N.tag(id))) N.log("trace", "remove " + describe(id) + "\n" + new Error().stack); remove(id); };
    const insert = N.insert;
    N.insert = (p, c, b) => { if (N.tag(c) === "script") N.log("trace", "insert <script type=" + N.getAttr(c, "type") + " src=" + N.getAttr(c, "src") + "> into <" + N.tag(p) + ">"); insert(p, c, b); };
    N.setText = (id, t) => { if (N.children(id).length > 3) N.log("trace", "setText " + describe(id) + "\n" + new Error().stack); setText(id, t); };
  }
  const wrappers = new Map();
  const isNode = (v) => v && typeof v === "object" && v.__id !== undefined;

  // ---- events ----------------------------------------------------------

  class Event {
    constructor(type, init) {
      init = init || {};
      this.type = String(type);
      this.bubbles = !!init.bubbles;
      this.cancelable = !!init.cancelable;
      this.composed = !!init.composed;
      this.defaultPrevented = false;
      this.target = null;
      this.currentTarget = null;
      this.eventPhase = 0;
      this.timeStamp = N.now();
      this.isTrusted = false;
      this._stop = false;
      this._stopNow = false;
    }
    preventDefault() { if (this.cancelable) this.defaultPrevented = true; }
    stopPropagation() { this._stop = true; }
    stopImmediatePropagation() { this._stop = true; this._stopNow = true; }
    composedPath() { return this._path || []; }
    get returnValue() { return !this.defaultPrevented; }
    set returnValue(v) { if (!v) this.preventDefault(); }
  }
  class CustomEvent extends Event {
    constructor(type, init) { super(type, init); this.detail = init && init.detail !== undefined ? init.detail : null; }
  }
  class UIEvent extends Event {}
  class MouseEvent extends UIEvent {
    constructor(type, init) {
      super(type, init); init = init || {};
      this.button = init.button || 0; this.buttons = init.buttons || 0;
      this.clientX = init.clientX || 0; this.clientY = init.clientY || 0;
      this.ctrlKey = !!init.ctrlKey; this.shiftKey = !!init.shiftKey; this.altKey = !!init.altKey; this.metaKey = !!init.metaKey;
    }
  }
  class KeyboardEvent extends UIEvent {
    constructor(type, init) { super(type, init); init = init || {}; this.key = init.key || ""; this.code = init.code || ""; }
  }
  class InputEvent extends UIEvent {}
  class FocusEvent extends UIEvent {}

  class EventTarget {
    addEventListener(type, fn, opts) {
      if (!fn) return;
      const capture = typeof opts === "boolean" ? opts : !!(opts && opts.capture);
      const once = !!(opts && typeof opts === "object" && opts.once);
      const l = this._listeners || (this._listeners = Object.create(null));
      const list = l[type] || (l[type] = []);
      if (list.some((e) => e.fn === fn && e.capture === capture)) return;
      list.push({ fn, capture, once });
      if (type === "click" && this.__id !== undefined) clickListeners.add(this.__id);
    }
    removeEventListener(type, fn, opts) {
      const capture = typeof opts === "boolean" ? opts : !!(opts && opts.capture);
      const list = this._listeners && this._listeners[type];
      if (!list) return;
      const i = list.findIndex((e) => e.fn === fn && e.capture === capture);
      if (i >= 0) list.splice(i, 1);
    }
    dispatchEvent(ev) {
      ev.target = this;
      const path = [];
      for (let n = this; n; n = parentForEvents(n)) path.push(n);
      ev._path = path;
      // Capture, target, bubble.
      for (let i = path.length - 1; i > 0 && !ev._stop; i--) invoke(path[i], ev, 1, true);
      if (!ev._stop) invoke(path[0], ev, 2, null);
      if (ev.bubbles) for (let i = 1; i < path.length && !ev._stop; i++) invoke(path[i], ev, 3, false);
      ev.currentTarget = null;
      ev.eventPhase = 0;
      return !ev.defaultPrevented;
    }
  }
  function parentForEvents(n) {
    if (n === window) return null;
    if (n === document) return window;
    return n.parentNode;
  }
  function invoke(node, ev, phase, capture) {
    ev.currentTarget = node;
    ev.eventPhase = phase;
    const list = node._listeners && node._listeners[ev.type];
    if (list) {
      for (const l of list.slice()) {
        if (capture !== null && l.capture !== capture) continue;
        if (l.once) node.removeEventListener(ev.type, l.fn, l.capture);
        try {
          const r = typeof l.fn === "function" ? l.fn.call(node, ev) : l.fn.handleEvent(ev);
          if (r === false) ev.preventDefault();
        } catch (e) { reportError(e); }
        if (ev._stopNow) return;
      }
    }
    // `onclick = ...` properties and onclick="..." attributes.
    if (capture !== true) {
      const handler = inlineHandler(node, ev.type);
      if (handler) {
        try { if (handler.call(node, ev) === false) ev.preventDefault(); } catch (e) { reportError(e); }
      }
    }
  }
  function inlineHandler(node, type) {
    const own = node["_on" + type];
    if (typeof own === "function") return own;
    if (node.__id === undefined || !node.getAttribute) return null;
    const code = node.getAttribute("on" + type);
    if (!code) return null;
    try { return new Function("event", code); } catch (e) { reportError(e); return null; }
  }
  function reportError(e) {
    N.log("error", (e && e.stack) ? e.stack : String(e));
  }
  const clickListeners = new Set();

  // ---- nodes -----------------------------------------------------------

  function wrap(id) {
    if (id === null || id === undefined) return null;
    let w = wrappers.get(id);
    if (w) return w;
    const type = N.nodeType(id);
    let proto;
    if (type === 1) {
      const tag = N.tag(id);
      proto = (elementClasses[tag] || HTMLElement).prototype;
    } else if (type === 3) proto = Text.prototype;
    else if (type === 8) proto = Comment.prototype;
    else if (type === 9) proto = Document.prototype;
    else if (type === 11) proto = DocumentFragment.prototype;
    else proto = Node.prototype;
    w = Object.create(proto);
    Object.defineProperty(w, "__id", { value: id });
    wrappers.set(id, w);
    return w;
  }
  const ids = (list) => list.map(wrap);
  function toNode(v) {
    if (isNode(v)) return v;
    return wrap(N.createText(String(v)));
  }

  class NodeList extends Array {
    item(i) { return this[i] || null; }
  }
  const nodeList = (arr) => { const l = new NodeList(); for (const x of arr) l.push(x); return l; };

  class Node extends EventTarget {
    get nodeType() { return N.nodeType(this.__id); }
    get nodeName() { const t = N.tag(this.__id); return t ? t.toUpperCase() : (this.nodeType === 3 ? "#text" : this.nodeType === 9 ? "#document" : "#node"); }
    get parentNode() { return wrap(N.parent(this.__id)); }
    get parentElement() { const p = this.parentNode; return p && p.nodeType === 1 ? p : null; }
    get childNodes() { return nodeList(ids(N.children(this.__id))); }
    get firstChild() { const c = N.children(this.__id); return wrap(c.length ? c[0] : null); }
    get lastChild() { const c = N.children(this.__id); return wrap(c.length ? c[c.length - 1] : null); }
    get nextSibling() { return sibling(this, 1, false); }
    get previousSibling() { return sibling(this, -1, false); }
    get ownerDocument() { return document; }
    get isConnected() { for (let n = this; n; n = n.parentNode) if (n === document) return true; return false; }
    get textContent() { return N.text(this.__id); }
    set textContent(v) { N.setText(this.__id, v === null || v === undefined ? "" : String(v)); }
    get nodeValue() { return this.nodeType === 1 ? null : N.text(this.__id); }
    set nodeValue(v) { if (this.nodeType !== 1) N.setText(this.__id, String(v)); }
    hasChildNodes() { return N.children(this.__id).length > 0; }
    contains(other) { for (let n = other; n; n = n.parentNode) if (n === this) return true; return false; }
    appendChild(c) { N.insert(this.__id, c.__id, null); return c; }
    insertBefore(c, ref) { N.insert(this.__id, c.__id, ref ? ref.__id : null); return c; }
    removeChild(c) { N.remove(c.__id); return c; }
    replaceChild(n, old) { N.insert(this.__id, n.__id, old.__id); N.remove(old.__id); return old; }
    cloneNode(deep) { return wrap(N.clone(this.__id, !!deep)); }
    isEqualNode(o) { return o === this; }
    isSameNode(o) { return o === this; }
    getRootNode() { let n = this; while (n.parentNode) n = n.parentNode; return n; }
    compareDocumentPosition(o) { return this.contains(o) ? 20 : o.contains(this) ? 10 : 4; }
  }
  Object.assign(Node, { ELEMENT_NODE: 1, TEXT_NODE: 3, COMMENT_NODE: 8, DOCUMENT_NODE: 9, DOCUMENT_FRAGMENT_NODE: 11 });

  function sibling(node, dir, elementsOnly) {
    const p = N.parent(node.__id);
    if (p === null) return null;
    const kids = N.children(p);
    let i = kids.indexOf(node.__id) + dir;
    while (i >= 0 && i < kids.length) {
      if (!elementsOnly || N.nodeType(kids[i]) === 1) return wrap(kids[i]);
      i += dir;
    }
    return null;
  }
  function elementChildren(id) { return N.children(id).filter((c) => N.nodeType(c) === 1); }

  // append/prepend/before/after/replaceWith/remove, children & queries.
  const ParentMixin = {
    get children() { return nodeList(ids(elementChildren(this.__id))); },
    get childElementCount() { return elementChildren(this.__id).length; },
    get firstElementChild() { const c = elementChildren(this.__id); return wrap(c.length ? c[0] : null); },
    get lastElementChild() { const c = elementChildren(this.__id); return wrap(c.length ? c[c.length - 1] : null); },
    append(...nodes) { for (const n of nodes) N.insert(this.__id, toNode(n).__id, null); },
    prepend(...nodes) { const first = N.children(this.__id)[0]; for (const n of nodes) N.insert(this.__id, toNode(n).__id, first === undefined ? null : first); },
    replaceChildren(...nodes) { for (const c of N.children(this.__id)) N.remove(c); this.append(...nodes); },
    querySelector(sel) { return wrap(N.query(this.__id, String(sel), false)); },
    querySelectorAll(sel) { return nodeList(ids(N.query(this.__id, String(sel), true))); },
    getElementsByTagName(tag) { return nodeList(ids(N.query(this.__id, tag === "*" ? "*" : String(tag), true))); },
    getElementsByClassName(names) {
      const sel = String(names).trim().split(/\s+/).filter(Boolean).map((c) => "." + cssEscape(c)).join("");
      return sel ? nodeList(ids(N.query(this.__id, sel, true))) : nodeList([]);
    },
  };
  const ChildMixin = {
    remove() { N.remove(this.__id); },
    before(...nodes) { const p = N.parent(this.__id); if (p !== null) for (const n of nodes) N.insert(p, toNode(n).__id, this.__id); },
    after(...nodes) {
      const p = N.parent(this.__id); if (p === null) return;
      const next = this.nextSibling;
      for (const n of nodes) N.insert(p, toNode(n).__id, next ? next.__id : null);
    },
    replaceWith(...nodes) { this.before(...nodes); this.remove(); },
    get nextElementSibling() { return sibling(this, 1, true); },
    get previousElementSibling() { return sibling(this, -1, true); },
  };
  function mixin(cls, ...sources) {
    for (const src of sources) for (const key of Object.getOwnPropertyNames(src)) {
      Object.defineProperty(cls.prototype, key, Object.getOwnPropertyDescriptor(src, key));
    }
  }
  function cssEscape(s) { return String(s).replace(/([^a-zA-Z0-9_\-])/g, "\\$1"); }

  class CharacterData extends Node {
    get data() { return N.text(this.__id); }
    set data(v) { N.setText(this.__id, String(v)); }
    get length() { return this.data.length; }
  }
  mixin(CharacterData, ChildMixin);
  class Text extends CharacterData {
    get wholeText() { return this.data; }
  }
  class Comment extends CharacterData {}

  class DOMTokenList {
    constructor(el, attr) { this._el = el; this._attr = attr; }
    _get() { return (this._el.getAttribute(this._attr) || "").split(/\s+/).filter(Boolean); }
    _set(list) { this._el.setAttribute(this._attr, list.join(" ")); }
    get length() { return this._get().length; }
    get value() { return this._el.getAttribute(this._attr) || ""; }
    set value(v) { this._el.setAttribute(this._attr, v); }
    item(i) { return this._get()[i] || null; }
    contains(t) { return this._get().includes(String(t)); }
    add(...tokens) { const l = this._get(); for (const t of tokens) if (!l.includes(t)) l.push(String(t)); this._set(l); }
    remove(...tokens) { this._set(this._get().filter((t) => !tokens.includes(t))); }
    toggle(t, force) {
      const has = this.contains(t);
      const want = force === undefined ? !has : !!force;
      if (want && !has) this.add(t); else if (!want && has) this.remove(t);
      return want;
    }
    replace(a, b) { const l = this._get(); const i = l.indexOf(a); if (i < 0) return false; l[i] = b; this._set(l); return true; }
    forEach(fn, self) { this._get().forEach(fn, self); }
    [Symbol.iterator]() { return this._get()[Symbol.iterator](); }
    toString() { return this.value; }
  }

  // element.style: the inline `style` attribute, read and written as
  // declarations (camelCase properties and setProperty/getPropertyValue).
  const kebab = (p) => p.startsWith("--") ? p : p.replace(/^(webkit|moz|ms)(?=[A-Z])/, "-$1").replace(/[A-Z]/g, (c) => "-" + c.toLowerCase());
  function parseDecls(text) {
    const out = [];
    for (const part of (text || "").split(";")) {
      const i = part.indexOf(":");
      if (i < 0) continue;
      const name = part.slice(0, i).trim();
      if (name) out.push([name.startsWith("--") ? name : name.toLowerCase(), part.slice(i + 1).trim()]);
    }
    return out;
  }
  function styleFor(el) {
    const decls = () => parseDecls(el.getAttribute("style"));
    const write = (list) => {
      const text = list.map(([k, v]) => k + ": " + v).join("; ");
      if (text) el.setAttribute("style", text); else el.removeAttribute("style");
    };
    const api = {
      getPropertyValue(name) { const d = decls().find(([k]) => k === name); return d ? d[1].replace(/\s*!important$/, "") : ""; },
      setProperty(name, value, priority) {
        const list = decls().filter(([k]) => k !== name);
        if (value !== null && value !== undefined && value !== "") list.push([name, String(value) + (priority ? " !important" : "")]);
        write(list);
      },
      removeProperty(name) { const old = api.getPropertyValue(name); write(decls().filter(([k]) => k !== name)); return old; },
      get cssText() { return el.getAttribute("style") || ""; },
      set cssText(v) { el.setAttribute("style", String(v)); },
      get length() { return decls().length; },
      item(i) { const d = decls()[i]; return d ? d[0] : ""; },
    };
    return new Proxy(api, {
      get(target, prop) {
        if (prop in target) return target[prop];
        if (typeof prop !== "string") return undefined;
        return target.getPropertyValue(kebab(prop));
      },
      set(target, prop, value) {
        if (prop === "cssText") { target.cssText = value; return true; }
        if (typeof prop === "string") target.setProperty(kebab(prop), value);
        return true;
      },
    });
  }

  class Element extends Node {
    get tagName() { return N.tag(this.__id).toUpperCase(); }
    get localName() { return N.tag(this.__id); }
    get namespaceURI() { return "http://www.w3.org/1999/xhtml"; }
    getAttribute(name) { return N.getAttr(this.__id, String(name)); }
    setAttribute(name, value) { N.setAttr(this.__id, String(name), String(value)); }
    removeAttribute(name) { N.removeAttr(this.__id, String(name)); }
    hasAttribute(name) { return N.getAttr(this.__id, String(name)) !== null; }
    toggleAttribute(name, force) {
      const has = this.hasAttribute(name);
      const want = force === undefined ? !has : !!force;
      if (want && !has) this.setAttribute(name, ""); else if (!want && has) this.removeAttribute(name);
      return want;
    }
    getAttributeNames() { return N.attrNames(this.__id); }
    get attributes() {
      const list = N.attrNames(this.__id).map((name) => ({ name, value: this.getAttribute(name), localName: name }));
      list.getNamedItem = (n) => list.find((a) => a.name === n) || null;
      return list;
    }
    getAttributeNS(_, name) { return this.getAttribute(name); }
    setAttributeNS(_, name, value) { this.setAttribute(name, value); }
    get id() { return this.getAttribute("id") || ""; }
    set id(v) { this.setAttribute("id", v); }
    get className() { return this.getAttribute("class") || ""; }
    set className(v) { this.setAttribute("class", v); }
    get classList() { return this._classList || (this._classList = new DOMTokenList(this, "class")); }
    get innerHTML() { return N.innerHTML(this.__id); }
    set innerHTML(html) {
      for (const c of N.children(this.__id)) N.remove(c);
      N.insert(this.__id, N.parseHTML(this.localName, String(html)), null);
    }
    get outerHTML() { return N.outerHTML(this.__id); }
    set outerHTML(html) {
      const p = N.parent(this.__id); if (p === null) return;
      N.insert(p, N.parseHTML("div", String(html)), this.__id);
      N.remove(this.__id);
    }
    insertAdjacentHTML(where, html) {
      const frag = N.parseHTML("div", String(html));
      insertAdjacent(this, where, frag);
    }
    insertAdjacentElement(where, el) { insertAdjacent(this, where, el.__id); return el; }
    insertAdjacentText(where, text) { insertAdjacent(this, where, N.createText(String(text))); }
    matches(sel) { return N.matches(this.__id, String(sel)); }
    webkitMatchesSelector(sel) { return this.matches(sel); }
    closest(sel) { for (let n = this; n && n.nodeType === 1; n = n.parentNode) if (n.matches(sel)) return n; return null; }
    getBoundingClientRect() { return { x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0, toJSON() { return this; } }; }
    getClientRects() { return []; }
    scrollIntoView() {}
    scrollTo() {}
    scroll() {}
    focus() {}
    blur() {}
    click() { this.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })); }
    attachShadow() { return this; }
    animate() { return { finished: Promise.resolve(), cancel() {}, play() {}, pause() {}, addEventListener() {} }; }
    get clientWidth() { return 0; }
    get clientHeight() { return 0; }
    get offsetWidth() { return 0; }
    get offsetHeight() { return 0; }
    get offsetTop() { return 0; }
    get offsetLeft() { return 0; }
    get offsetParent() { return null; }
    get scrollTop() { return 0; }
    set scrollTop(_) {}
    get scrollLeft() { return 0; }
    set scrollLeft(_) {}
    get scrollWidth() { return 0; }
    get scrollHeight() { return 0; }
  }
  mixin(Element, ParentMixin, ChildMixin);
  function insertAdjacent(el, where, id) {
    switch (String(where).toLowerCase()) {
      case "beforebegin": { const p = N.parent(el.__id); if (p !== null) N.insert(p, id, el.__id); break; }
      case "afterbegin": { const first = N.children(el.__id)[0]; N.insert(el.__id, id, first === undefined ? null : first); break; }
      case "beforeend": N.insert(el.__id, id, null); break;
      case "afterend": { const p = N.parent(el.__id); if (p === null) break; const next = el.nextSibling; N.insert(p, id, next ? next.__id : null); break; }
    }
  }

  // Properties that reflect attributes.
  function reflect(cls, props) {
    for (const [prop, attr, kind] of props) {
      Object.defineProperty(cls.prototype, prop, {
        configurable: true,
        get() {
          const v = this.getAttribute(attr);
          if (kind === "bool") return v !== null;
          if (kind === "url") return v === null ? "" : (N.resolve(v) || v);
          if (kind === "num") return v === null ? 0 : Number(v);
          return v === null ? "" : v;
        },
        set(v) {
          if (kind === "bool") { if (v) this.setAttribute(attr, ""); else this.removeAttribute(attr); }
          else this.setAttribute(attr, String(v));
        },
      });
    }
  }

  class HTMLElement extends Element {
    get style() { return this._style || (this._style = styleFor(this)); }
    set style(v) { this.setAttribute("style", String(v)); }
    get dataset() {
      const el = this;
      return this._dataset || (this._dataset = new Proxy({}, {
        get(_, k) { if (typeof k !== "string") return undefined; const v = el.getAttribute("data-" + kebab(k)); return v === null ? undefined : v; },
        set(_, k, v) { el.setAttribute("data-" + kebab(k), String(v)); return true; },
        deleteProperty(_, k) { el.removeAttribute("data-" + kebab(k)); return true; },
        has(_, k) { return el.hasAttribute("data-" + kebab(k)); },
        ownKeys() { return el.getAttributeNames().filter((n) => n.startsWith("data-")).map((n) => n.slice(5).replace(/-([a-z])/g, (_, c) => c.toUpperCase())); },
        getOwnPropertyDescriptor(_, k) { const v = el.getAttribute("data-" + kebab(k)); return v === null ? undefined : { value: v, enumerable: true, configurable: true }; },
      }));
    }
    get innerText() { return this.textContent; }
    set innerText(v) { this.textContent = v; }
    get outerText() { return this.textContent; }
  }
  reflect(HTMLElement, [["title", "title"], ["lang", "lang"], ["dir", "dir"], ["hidden", "hidden", "bool"], ["tabIndex", "tabindex", "num"], ["accessKey", "accesskey"], ["contentEditable", "contenteditable"], ["draggable", "draggable", "bool"]]);
  for (const ev of ["click", "input", "change", "submit", "load", "error", "keydown", "keyup", "focus", "blur", "mouseover", "mouseout", "mousedown", "mouseup", "scroll", "resize"]) {
    for (const target of [HTMLElement.prototype, EventTarget.prototype]) {
      Object.defineProperty(target, "on" + ev, {
        configurable: true,
        get() { return this["_on" + ev] || null; },
        set(fn) { this["_on" + ev] = fn; if (ev === "click" && this.__id !== undefined) clickListeners.add(this.__id); },
      });
    }
  }

  class HTMLAnchorElement extends HTMLElement {
    toString() { return this.href; }
    get origin() { return new URL(this.href).origin; }
    get pathname() { return new URL(this.href).pathname; }
    get hostname() { return new URL(this.href).hostname; }
    get host() { return new URL(this.href).host; }
    get protocol() { return new URL(this.href).protocol; }
    get search() { return new URL(this.href).search; }
    get hash() { return new URL(this.href).hash; }
  }
  reflect(HTMLAnchorElement, [["href", "href", "url"], ["target", "target"], ["rel", "rel"], ["download", "download"]]);
  class HTMLImageElement extends HTMLElement {
    get complete() { return true; }
    get naturalWidth() { return Number(this.getAttribute("width")) || 0; }
    get naturalHeight() { return Number(this.getAttribute("height")) || 0; }
    decode() { return Promise.resolve(); }
  }
  reflect(HTMLImageElement, [["src", "src", "url"], ["alt", "alt"], ["srcset", "srcset"], ["width", "width", "num"], ["height", "height", "num"], ["loading", "loading"]]);
  class HTMLScriptElement extends HTMLElement {
    get text() { return this.textContent; }
    set text(v) { this.textContent = v; }
  }
  reflect(HTMLScriptElement, [["src", "src", "url"], ["type", "type"], ["async", "async", "bool"], ["defer", "defer", "bool"], ["crossOrigin", "crossorigin"], ["noModule", "nomodule", "bool"]]);
  class HTMLLinkElement extends HTMLElement {}
  reflect(HTMLLinkElement, [["href", "href", "url"], ["rel", "rel"], ["as", "as"], ["media", "media"], ["type", "type"]]);
  class HTMLStyleElement extends HTMLElement {
    get sheet() { return { cssRules: [], insertRule() { return 0; }, deleteRule() {} }; }
  }

  // Form fields: `value` lives in the tab's widget (the page tells us as
  // it changes); writing it updates the widget through the attribute.
  class HTMLInputElement extends HTMLElement {
    get value() { const v = N.value(this.__id); return v !== null ? v : (this.getAttribute("value") || ""); }
    set value(v) { N.setValue(this.__id, String(v)); }
    get checked() { return this.hasAttribute("checked"); }
    set checked(v) { if (v) this.setAttribute("checked", ""); else this.removeAttribute("checked"); }
    get form() { return this.closest("form"); }
    select() {}
    setSelectionRange() {}
    checkValidity() { return true; }
    reportValidity() { return true; }
    setCustomValidity() {}
    get validity() { return { valid: true }; }
  }
  reflect(HTMLInputElement, [["type", "type"], ["name", "name"], ["placeholder", "placeholder"], ["disabled", "disabled", "bool"], ["required", "required", "bool"], ["readOnly", "readonly", "bool"], ["defaultValue", "value"], ["min", "min"], ["max", "max"], ["step", "step"], ["autocomplete", "autocomplete"]]);
  class HTMLTextAreaElement extends HTMLInputElement {
    get value() { const v = N.value(this.__id); return v !== null ? v : this.textContent; }
    set value(v) { N.setValue(this.__id, String(v)); }
  }
  class HTMLSelectElement extends HTMLElement {
    get options() { return this.querySelectorAll("option"); }
    get selectedIndex() { const o = Array.from(this.options); const i = o.findIndex((x) => x.hasAttribute("selected")); return i < 0 && o.length ? 0 : i; }
    set selectedIndex(i) { Array.from(this.options).forEach((o, j) => { if (j === i) o.setAttribute("selected", ""); else o.removeAttribute("selected"); }); }
    get value() { const o = this.options[this.selectedIndex]; return o ? o.value : ""; }
    set value(v) { this.selectedIndex = Array.from(this.options).findIndex((o) => o.value === String(v)); }
    get form() { return this.closest("form"); }
  }
  reflect(HTMLSelectElement, [["name", "name"], ["disabled", "disabled", "bool"], ["multiple", "multiple", "bool"]]);
  class HTMLOptionElement extends HTMLElement {
    get value() { const v = this.getAttribute("value"); return v === null ? this.textContent.trim() : v; }
    set value(v) { this.setAttribute("value", v); }
    get selected() { return this.hasAttribute("selected"); }
    set selected(v) { if (v) this.setAttribute("selected", ""); else this.removeAttribute("selected"); }
    get text() { return this.textContent; }
  }
  class HTMLButtonElement extends HTMLElement {
    get form() { return this.closest("form"); }
  }
  reflect(HTMLButtonElement, [["type", "type"], ["name", "name"], ["value", "value"], ["disabled", "disabled", "bool"]]);
  class HTMLFormElement extends HTMLElement {
    get elements() { return this.querySelectorAll("input, select, textarea, button"); }
    submit() { N.navigate(this.action || location.href); }
    requestSubmit() { if (this.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }))) this.submit(); }
    reset() {}
  }
  reflect(HTMLFormElement, [["action", "action", "url"], ["method", "method"], ["name", "name"], ["target", "target"]]);
  class HTMLTemplateElement extends HTMLElement {
    get content() { const f = document.createDocumentFragment(); for (const c of this.childNodes) f.appendChild(c.cloneNode(true)); return f; }
  }
  class HTMLCanvasElement extends HTMLElement {
    getContext() { return null; }
    toDataURL() { return "data:,"; }
  }
  class HTMLMediaElement extends HTMLElement {
    play() { return Promise.resolve(); }
    pause() {}
    load() {}
    canPlayType() { return ""; }
  }
  class HTMLIFrameElement extends HTMLElement {
    get contentWindow() { return null; }
    get contentDocument() { return null; }
  }
  const elementClasses = {
    a: HTMLAnchorElement, img: HTMLImageElement, script: HTMLScriptElement, link: HTMLLinkElement, style: HTMLStyleElement,
    input: HTMLInputElement, textarea: HTMLTextAreaElement, select: HTMLSelectElement, option: HTMLOptionElement,
    button: HTMLButtonElement, form: HTMLFormElement, template: HTMLTemplateElement, canvas: HTMLCanvasElement,
    video: HTMLMediaElement, audio: HTMLMediaElement, iframe: HTMLIFrameElement,
  };

  class DocumentFragment extends Node {
    getElementById(id) { return this.querySelector("#" + cssEscape(id)); }
  }
  mixin(DocumentFragment, ParentMixin);

  let readyState = "loading";
  let currentScript = null;
  const cookies = Object.create(null);
  class Document extends Node {
    get documentElement() { return wrap(elementChildren(this.__id)[0] === undefined ? null : elementChildren(this.__id)[0]); }
    get head() { return this.querySelector("head"); }
    get body() { return this.querySelector("body"); }
    get title() { const t = this.querySelector("title"); return t ? t.textContent.trim() : ""; }
    set title(v) {
      let t = this.querySelector("title");
      if (!t) { t = this.createElement("title"); const h = this.head; if (h) h.appendChild(t); }
      t.textContent = v;
    }
    get readyState() { return readyState; }
    get currentScript() { return currentScript; }
    get URL() { return N.url(); }
    get documentURI() { return N.url(); }
    get baseURI() { return N.url(); }
    get location() { return location; }
    set location(v) { location.href = v; }
    get defaultView() { return window; }
    get referrer() { return ""; }
    get domain() { return location.hostname; }
    get cookie() { return Object.entries(cookies).map(([k, v]) => k + "=" + v).join("; "); }
    set cookie(v) {
      const pair = String(v).split(";")[0];
      const i = pair.indexOf("=");
      if (i > 0) cookies[pair.slice(0, i).trim()] = pair.slice(i + 1).trim();
    }
    get visibilityState() { return "visible"; }
    get hidden() { return false; }
    get compatMode() { return "CSS1Compat"; }
    get characterSet() { return "UTF-8"; }
    get contentType() { return "text/html"; }
    get activeElement() { return this.body; }
    get forms() { return this.querySelectorAll("form"); }
    get images() { return this.querySelectorAll("img"); }
    get links() { return this.querySelectorAll("a[href]"); }
    get scripts() { return this.querySelectorAll("script"); }
    get implementation() {
      return {
        hasFeature() { return true; },
        createHTMLDocument(title) {
          const d = wrap(N.createDocument());
          if (title !== undefined) d.title = title;
          return d;
        },
      };
    }
    getElementById(id) { return this.querySelector("#" + cssEscape(id)); }
    getElementsByName(name) { return this.querySelectorAll("[name=\"" + String(name).replace(/"/g, "\\\"") + "\"]"); }
    createElement(tag) { return wrap(N.createElement(String(tag))); }
    createElementNS(_, tag) { return wrap(N.createElement(String(tag))); }
    createTextNode(text) { return wrap(N.createText(String(text))); }
    createComment(text) { return wrap(N.createComment(String(text))); }
    createDocumentFragment() { return wrap(N.createFragment()); }
    createEvent() { return new Event(""); }
    createRange() {
      return { selectNodeContents() {}, setStart() {}, setEnd() {}, collapse() {}, getBoundingClientRect() { return { top: 0, left: 0, width: 0, height: 0 }; }, createContextualFragment: (html) => wrap(N.parseHTML("div", String(html))) };
    }
    createTreeWalker(root) {
      const all = [root, ...root.querySelectorAll("*")];
      let i = 0;
      return { currentNode: root, nextNode() { i++; return (this.currentNode = all[i] || null); } };
    }
    importNode(n, deep) { return n.cloneNode(deep); }
    adoptNode(n) { return n; }
    hasFocus() { return true; }
    // After the page is parsed, write() can only append (it's mostly ads and analytics).
    write(...html) {
      const target = currentScript && currentScript.parentNode ? currentScript : null;
      const frag = N.parseHTML("body", html.join(""));
      if (target) N.insert(N.parent(target.__id), frag, target.nextSibling ? target.nextSibling.__id : null);
      else if (this.body) N.insert(this.body.__id, frag, null);
    }
    writeln(...html) { this.write(...html, "\n"); }
    open() { return this; }
    close() {}
    execCommand() { return false; }
    getSelection() { return { rangeCount: 0, removeAllRanges() {}, addRange() {}, toString() { return ""; } }; }
    elementFromPoint() { return null; }
    querySelector(sel) { return wrap(N.query(this.__id, String(sel), false)); }
  }
  mixin(Document, { get children() { return nodeList(ids(elementChildren(this.__id))); } });
  for (const key of ["querySelectorAll", "getElementsByTagName", "getElementsByClassName", "append", "prepend"]) {
    Object.defineProperty(Document.prototype, key, Object.getOwnPropertyDescriptor(ParentMixin, key));
  }

  const document = wrap(N.root());

  // ---- window ----------------------------------------------------------

  const window = global;
  class Window extends EventTarget {}
  Object.setPrototypeOf(window, Window.prototype);


  // Timers: callbacks here, deadlines in Rust.
  const timers = new Map();
  let nextTimer = 1;
  function addTimer(fn, ms, args, repeat) {
    const id = nextTimer++;
    const delay = Math.max(0, Number(ms) || 0);
    timers.set(id, { fn, args, repeat, delay });
    N.setTimer(id, delay);
    return id;
  }
  function clearTimer(id) { timers.delete(id); N.clearTimer(id); }
  function fireTimer(id) {
    const t = timers.get(id);
    if (!t) return;
    if (t.repeat) N.setTimer(id, Math.max(t.delay, 4)); else timers.delete(id);
    try {
      if (typeof t.fn === "function") t.fn(...(t.args || [])); else (0, eval)(String(t.fn));
    } catch (e) { reportError(e); }
  }

  const storage = () => {
    const data = new Map();
    return {
      getItem: (k) => (data.has(String(k)) ? data.get(String(k)) : null),
      setItem: (k, v) => { data.set(String(k), String(v)); },
      removeItem: (k) => { data.delete(String(k)); },
      clear: () => data.clear(),
      key: (i) => Array.from(data.keys())[i] || null,
      get length() { return data.size; },
    };
  };

  // A small URL implementation (enough for the common uses).
  class URLSearchParams {
    constructor(init) {
      this._list = [];
      if (typeof init === "string") {
        for (const part of init.replace(/^\?/, "").split("&")) {
          if (!part) continue;
          const i = part.indexOf("=");
          const dec = (s) => decodeURIComponent(s.replace(/\+/g, " "));
          this._list.push(i < 0 ? [dec(part), ""] : [dec(part.slice(0, i)), dec(part.slice(i + 1))]);
        }
      } else if (init && typeof init === "object") {
        for (const [k, v] of (Symbol.iterator in init ? init : Object.entries(init))) this._list.push([String(k), String(v)]);
      }
    }
    get(k) { const e = this._list.find(([a]) => a === k); return e ? e[1] : null; }
    getAll(k) { return this._list.filter(([a]) => a === k).map(([, v]) => v); }
    has(k) { return this._list.some(([a]) => a === k); }
    set(k, v) { const i = this._list.findIndex(([a]) => a === k); if (i < 0) this._list.push([k, String(v)]); else { this._list[i][1] = String(v); this._list = this._list.filter(([a], j) => a !== k || j === i); } }
    append(k, v) { this._list.push([k, String(v)]); }
    delete(k) { this._list = this._list.filter(([a]) => a !== k); }
    forEach(fn) { this._list.forEach(([k, v]) => fn(v, k, this)); }
    keys() { return this._list.map(([k]) => k)[Symbol.iterator](); }
    values() { return this._list.map(([, v]) => v)[Symbol.iterator](); }
    entries() { return this._list.map((e) => e.slice())[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
    toString() { return this._list.map(([k, v]) => encodeURIComponent(k) + "=" + encodeURIComponent(v)).join("&"); }
  }
  class URL {
    constructor(href, base) {
      const abs = N.resolveAgainst(String(href), base === undefined ? null : String(base));
      if (abs === null) throw new TypeError("Invalid URL: " + href);
      this._parse(abs);
    }
    _parse(abs) {
      const m = /^([a-zA-Z][a-zA-Z0-9+.-]*:)(?:\/\/(?:([^:@\/]*)(?::([^@\/]*))?@)?([^:\/?#]*)(?::(\d+))?)?([^?#]*)(\?[^#]*)?(#.*)?$/.exec(abs) || [];
      this.protocol = m[1] || ""; this.username = m[2] || ""; this.password = m[3] || "";
      this.hostname = m[4] || ""; this.port = m[5] || ""; this.pathname = m[6] || "/";
      this.search = m[7] && m[7] !== "?" ? m[7] : ""; this.hash = m[8] && m[8] !== "#" ? m[8] : "";
    }
    get host() { return this.hostname + (this.port ? ":" + this.port : ""); }
    get origin() { return this.protocol + "//" + this.host; }
    get href() { return this.protocol + (this.hostname || this.protocol === "file:" ? "//" : "") + (this.username ? this.username + (this.password ? ":" + this.password : "") + "@" : "") + this.host + this.pathname + this.search + this.hash; }
    set href(v) { this._parse(N.resolveAgainst(String(v), null)); }
    get searchParams() {
      const p = new URLSearchParams(this.search);
      const self = this;
      for (const m of ["set", "append", "delete"]) { const f = p[m].bind(p); p[m] = (...a) => { f(...a); const s = p.toString(); self.search = s ? "?" + s : ""; }; }
      return p;
    }
    toString() { return this.href; }
    toJSON() { return this.href; }
    static createObjectURL() { return "blob:feather"; }
    static revokeObjectURL() {}
    static canParse(h, b) { try { new URL(h, b); return true; } catch (_) { return false; } }
  }

  const location = {
    get href() { return N.url(); },
    set href(v) { N.navigate(String(v)); },
    assign(v) { N.navigate(String(v)); },
    replace(v) { N.navigate(String(v)); },
    reload() { N.navigate(N.url()); },
    toString() { return N.url(); },
  };
  for (const k of ["protocol", "host", "hostname", "port", "pathname", "search", "hash", "origin"]) {
    Object.defineProperty(location, k, {
      get() { return new URL(N.url())[k]; },
      set(v) { const u = new URL(N.url()); u[k] = v; if (k !== "hash") N.navigate(u.href); },
    });
  }

  const history = {
    length: 1, state: null,
    pushState(state) { this.state = state; },
    replaceState(state) { this.state = state; },
    back() {}, forward() {}, go() {},
  };

  function matchMedia(query) {
    const q = String(query);
    let matches = false;
    if (/prefers-color-scheme:\s*dark/.test(q)) matches = N.dark();
    else if (/prefers-color-scheme:\s*light/.test(q)) matches = !N.dark();
    else if (/min-width:\s*(\d+)/.test(q)) matches = innerWidth >= Number(RegExp.$1);
    else if (/max-width:\s*(\d+)/.test(q)) matches = innerWidth <= Number(RegExp.$1);
    return { matches, media: q, onchange: null, addListener() {}, removeListener() {}, addEventListener() {}, removeEventListener() {} };
  }

  const observer = (name) => class {
    constructor(cb) { this._cb = cb; }
    observe(target) {
      // Everything is visible right away (lazy loaders show their content).
      if (name === "IntersectionObserver") {
        const cb = this._cb, self = this;
        setTimeout(() => cb([{ target, isIntersecting: true, intersectionRatio: 1, boundingClientRect: target.getBoundingClientRect(), intersectionRect: {}, rootBounds: null, time: N.now() }], self), 0);
      }
    }
    unobserve() {} disconnect() {} takeRecords() { return []; }
  };

  const consoleImpl = {};
  const fmt = (args) => args.map((a) => {
    if (typeof a === "string") return a;
    if (a instanceof Error) return a.stack || String(a);
    if (isNode(a)) return "<" + (a.localName || a.nodeName) + ">";
    try { return JSON.stringify(a); } catch (_) { return String(a); }
  }).join(" ");
  for (const level of ["log", "info", "warn", "error", "debug", "trace"]) consoleImpl[level] = (...a) => N.log(level, fmt(a));
  for (const name of ["group", "groupCollapsed", "groupEnd", "time", "timeEnd", "count", "assert", "table", "dir"]) consoleImpl[name] = () => {};

  const navigatorImpl = {
    userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Feather/0.1 Safari/537.36",
    language: "en-US", languages: ["en-US", "en"], platform: "MacIntel", vendor: "", onLine: true,
    cookieEnabled: true, hardwareConcurrency: 8, maxTouchPoints: 0, webdriver: false,
    sendBeacon() { return true; },
    clipboard: { writeText: () => Promise.resolve(), readText: () => Promise.resolve("") },
    serviceWorker: undefined,
  };

  Object.assign(window, {
    window, self: window, globalThis: window, top: window, parent: window, frames: window,
    document, location, history, navigator: navigatorImpl, console: consoleImpl,
    Window, HTMLDocument: Document,
    Node, Element, HTMLElement, Text, Comment, Document, DocumentFragment, CharacterData, EventTarget,
    HTMLAnchorElement, HTMLImageElement, HTMLScriptElement, HTMLLinkElement, HTMLStyleElement, HTMLInputElement,
    HTMLTextAreaElement, HTMLSelectElement, HTMLOptionElement, HTMLButtonElement, HTMLFormElement,
    HTMLTemplateElement, HTMLCanvasElement, HTMLMediaElement, HTMLVideoElement: HTMLMediaElement, HTMLAudioElement: HTMLMediaElement,
    HTMLIFrameElement, HTMLDivElement: HTMLElement, HTMLSpanElement: HTMLElement, SVGElement: Element,
    NodeList, HTMLCollection: NodeList, DOMTokenList,
    Event, CustomEvent, UIEvent, MouseEvent, KeyboardEvent, InputEvent, FocusEvent, PointerEvent: MouseEvent,
    URL, URLSearchParams,
    DOMParser: class {
      parseFromString(html) {
        const d = wrap(N.createDocument());
        d.body.innerHTML = String(html);
        return d;
      }
    },
    setTimeout: (fn, ms, ...args) => addTimer(fn, ms, args, false),
    setInterval: (fn, ms, ...args) => addTimer(fn, ms, args, true),
    clearTimeout: clearTimer, clearInterval: clearTimer,
    requestAnimationFrame: (fn) => addTimer(() => fn(N.now()), 16, [], false),
    cancelAnimationFrame: clearTimer,
    requestIdleCallback: (fn) => addTimer(() => fn({ didTimeout: false, timeRemaining: () => 10 }), 1, [], false),
    cancelIdleCallback: clearTimer,
    queueMicrotask: (fn) => { Promise.resolve().then(fn); },
    performance: { now: () => N.now(), timeOrigin: Date.now(), mark() {}, measure() {}, getEntriesByType() { return []; }, getEntriesByName() { return []; } },
    localStorage: storage(), sessionStorage: storage(),
    matchMedia,
    getComputedStyle: (el) => el.style,
    MutationObserver: observer("MutationObserver"),
    IntersectionObserver: observer("IntersectionObserver"),
    ResizeObserver: observer("ResizeObserver"),
    PerformanceObserver: observer("PerformanceObserver"),
    innerWidth: 1280, innerHeight: 800, outerWidth: 1280, outerHeight: 900, devicePixelRatio: 2,
    screen: { width: 1512, height: 982, availWidth: 1512, availHeight: 950, colorDepth: 24 },
    scrollX: 0, scrollY: 0, pageXOffset: 0, pageYOffset: 0,
    scrollTo() {}, scrollBy() {}, scroll() {},
    alert() {}, confirm() { return false; }, prompt() { return null; },
    open() { return null; }, close() {}, focus() {}, blur() {}, print() {},
    postMessage() {},
    atob: (s) => base64Decode(String(s)), btoa: (s) => base64Encode(String(s)),
    reportError,
    isSecureContext: true,
    crypto: {
      getRandomValues(arr) { for (let i = 0; i < arr.length; i++) arr[i] = Math.floor(Math.random() * 256); return arr; },
      randomUUID() { return "10000000-1000-4000-8000-100000000000".replace(/[018]/g, (c) => (c ^ Math.floor(Math.random() * 16) >> c / 4).toString(16)); },
    },
    customElements: { define() {}, get() { return undefined; }, whenDefined() { return Promise.resolve(); } },
  });

  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  function base64Encode(s) {
    let out = "";
    for (let i = 0; i < s.length; i += 3) {
      const a = s.charCodeAt(i), b = s.charCodeAt(i + 1), c = s.charCodeAt(i + 2);
      out += B64[a >> 2] + B64[((a & 3) << 4) | (b >> 4 || 0)] + (isNaN(b) ? "=" : B64[((b & 15) << 2) | (c >> 6 || 0)]) + (isNaN(c) ? "=" : B64[c & 63]);
    }
    return out;
  }
  function base64Decode(s) {
    s = s.replace(/[^A-Za-z0-9+/]/g, "");
    let out = "";
    for (let i = 0; i < s.length; i += 4) {
      const n = (B64.indexOf(s[i]) << 18) | (B64.indexOf(s[i + 1]) << 12) | ((B64.indexOf(s[i + 2]) & 63) << 6) | (B64.indexOf(s[i + 3]) & 63);
      out += String.fromCharCode((n >> 16) & 255);
      if (s[i + 2] !== undefined) out += String.fromCharCode((n >> 8) & 255);
      if (s[i + 3] !== undefined) out += String.fromCharCode(n & 255);
    }
    return out;
  }

  // ---- hooks for the host ----------------------------------------------

  Object.assign(window, {
    __setCurrentScript(id) { currentScript = wrap(id); },
    __setReadyState(s) { readyState = s; document.dispatchEvent(new Event("readystatechange")); },
    __fireDocument(type) { document.dispatchEvent(new Event(type, { bubbles: true })); },
    __fireWindow(type) { window.dispatchEvent(new Event(type)); },
    __fireNode(id, type) { const n = wrap(id); if (n) n.dispatchEvent(new Event(type)); },
    __fireTimer: fireTimer,
    __dispatchClick(id) {
      const n = wrap(id);
      if (!n) return false;
      const ev = new MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
      ev.isTrusted = true;
      return !n.dispatchEvent(ev);
    },
    __dispatchInput(id) {
      const n = wrap(id);
      if (!n) return;
      n.dispatchEvent(new InputEvent("input", { bubbles: true }));
    },
    __listeningIds() {
      const out = [];
      for (const id of clickListeners) {
        const w = wrappers.get(id);
        if (w && ((w._listeners && w._listeners.click && w._listeners.click.length) || w._onclick)) out.push(id);
      }
      for (const n of N.query(N.root(), "[onclick]", true)) out.push(n);
      return out;
    },
  });
})(globalThis);
