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
  // Intl.RelativeTimeFormat (Boa lacks it): English, "in 3 days" / "3
  // days ago", numeric: "auto" says "yesterday", "next week"...
  if (typeof Intl !== "undefined" && !Intl.RelativeTimeFormat) {
    const UNITS = ["year", "quarter", "month", "week", "day", "hour", "minute", "second"];
    const AUTO = {
      day: { "-1": "yesterday", "0": "today", "1": "tomorrow" },
      year: { "-1": "last year", "0": "this year", "1": "next year" },
      quarter: { "-1": "last quarter", "0": "this quarter", "1": "next quarter" },
      month: { "-1": "last month", "0": "this month", "1": "next month" },
      week: { "-1": "last week", "0": "this week", "1": "next week" },
      hour: { "0": "this hour" }, minute: { "0": "this minute" }, second: { "0": "now" },
    };
    const SHORT = { year: "yr.", quarter: "qtr.", month: "mo.", week: "wk.", day: "day", hour: "hr.", minute: "min.", second: "sec." };
    class RelativeTimeFormat {
      constructor(locale, options) {
        this._o = Object.assign({ numeric: "always", style: "long", localeMatcher: "best fit" }, options || {});
        this._locale = Array.isArray(locale) ? locale[0] || "en-US" : locale || "en-US";
      }
      static supportedLocalesOf(l) { return (Array.isArray(l) ? l : [l]).filter(Boolean); }
      resolvedOptions() { return { locale: this._locale, numberingSystem: "latn", ...this._o }; }
      _unit(unit) {
        const u = String(unit).replace(/s$/, "");
        if (!UNITS.includes(u)) throw new RangeError("Invalid unit argument for format() '" + unit + "'");
        return u;
      }
      formatToParts(value, unit) {
        const u = this._unit(unit);
        const v = Number(value);
        if (!isFinite(v)) throw new RangeError("Invalid value");
        if (this._o.numeric === "auto" && AUTO[u] && AUTO[u][String(v)] !== undefined) return [{ type: "literal", value: AUTO[u][String(v)] }];
        const n = Math.abs(v);
        const num = new Intl.NumberFormat(this._locale).format(n);
        const name = this._o.style === "long" ? u + (n === 1 ? "" : "s") : SHORT[u];
        const past = v < 0 || Object.is(v, -0);
        return past
          ? [{ type: "integer", value: num, unit: u }, { type: "literal", value: " " + name + " ago" }]
          : [{ type: "literal", value: "in " }, { type: "integer", value: num, unit: u }, { type: "literal", value: " " + name }];
      }
      format(value, unit) { return this.formatToParts(value, unit).map((p) => p.value).join(""); }
    }
    Intl.RelativeTimeFormat = RelativeTimeFormat;
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
  // Date strings Chrome reads and Boa doesn't: microseconds, "2026-10-26
  // 17:36", slashes, month names ("Oct 26 2026 17:36 GMT+0000").
  {
    const NativeDate = Date;
    const nativeParse = Date.parse;
    const MONTH = { jan: 0, feb: 1, mar: 2, apr: 3, may: 4, jun: 5, jul: 6, aug: 7, sep: 8, oct: 9, nov: 10, dec: 11 };
    const lenient = (text) => {
      let s = String(text).trim();
      // ISO with more than milliseconds.
      let m = /^(\d{4}-\d\d-\d\d[T ]\d\d:\d\d(?::\d\d)?)(?:\.(\d+))?(Z|[+-]\d\d:?\d\d)?$/i.exec(s);
      if (m) {
        const frac = m[2] ? "." + m[2].slice(0, 3).padEnd(3, "0") : "";
        const zone = m[3] ? (m[3].length === 5 ? m[3].slice(0, 3) + ":" + m[3].slice(3) : m[3]) : "";
        return nativeParse(m[1].replace(" ", "T") + frac + zone);
      }
      m = /^(\d{4})\/(\d{1,2})\/(\d{1,2})(?:[ T](\d{1,2}):(\d\d)(?::(\d\d))?)?$/.exec(s);
      if (m) return new NativeDate(+m[1], m[2] - 1, +m[3], +(m[4] || 0), +(m[5] || 0), +(m[6] || 0)).getTime();
      // Words: month name, day, year, time, zone, in the usual orders.
      const words = s.replace(/,/g, " ").split(/\s+/).filter(Boolean);
      let year, month, day, h = 0, min = 0, sec = 0, zone = null;
      for (const w of words) {
        const lower = w.toLowerCase();
        if (MONTH[lower.slice(0, 3)] !== undefined && /^[a-z]+\.?$/i.test(w)) month = MONTH[lower.slice(0, 3)];
        else if (/^\d{1,2}:\d\d(:\d\d)?$/.test(w)) [h, min, sec = 0] = w.split(":").map(Number);
        else if (/^\d{4}$/.test(w)) year = +w;
        else if (/^\d{1,2}(st|nd|rd|th)?$/i.test(w) && day === undefined) day = parseInt(w, 10);
        else if (/^(gmt|utc|z)([+-]\d{2}:?\d{2})?$/i.test(w) || /^[+-]\d{4}$/.test(w)) {
          const off = /([+-])(\d\d):?(\d\d)/.exec(w);
          zone = off ? (off[1] === "-" ? -1 : 1) * (+off[2] * 60 + +off[3]) : 0;
        } else if (/^(am|pm)$/i.test(w)) { if (lower === "pm" && h < 12) h += 12; if (lower === "am" && h === 12) h = 0; }
      }
      if (year === undefined || month === undefined) return NaN;
      if (zone === null) return new NativeDate(year, month, day || 1, h, min, sec).getTime();
      return NativeDate.UTC(year, month, day || 1, h, min, sec) - zone * 60000;
    };
    const parse = (text) => {
      const t = nativeParse(text);
      return isNaN(t) ? lenient(text) : t;
    };
    global.Date = new Proxy(NativeDate, {
      construct(target, args, newTarget) {
        if (args.length === 1 && typeof args[0] === "string") args = [parse(args[0])];
        return Reflect.construct(target, args, newTarget);
      },
    });
    NativeDate.parse = parse;
    // Dates' constructor is the Date pages see (`d.constructor === Date`).
    Object.defineProperty(NativeDate.prototype, "constructor", { value: global.Date, writable: true, configurable: true });
  }
  // Boa prints classes as `function X() { [native code] }`; frameworks tell
  // classes from factories by the source starting with `class`. Calling a
  // class without `new` throws before running anything, so that tells.
  {
    const toString = Function.prototype.toString;
    const classes = new WeakMap();
    const isClass = (f) => {
      let known = classes.get(f);
      if (known === undefined) {
        try { Reflect.apply(f, undefined, []); known = false; } catch (e) { known = e instanceof TypeError && /class constructor/.test(e.message); }
        classes.set(f, known);
      }
      return known;
    };
    Object.defineProperty(Function.prototype, "toString", {
      configurable: true, writable: true,
      value: function () {
        const text = toString.call(this);
        if (typeof this === "function" && this.prototype && text.endsWith("{ [native code] }") && isClass(this)) return "class " + (this.name || "") + " { [native code] }";
        return text;
      },
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
  // ---- MutationObserver: records made at the native mutations, delivered
  // in a microtask (Polymer runs its own microtasks this way).
  const mutationRegs = new Map(); // node id -> [{ observer, options }]
  const pendingObservers = new Set();
  let deliveryQueued = false;
  function interested(target, type, name) {
    let out = null;
    for (let n = target, self = true; n !== null && n !== undefined; n = N.parent(n), self = false) {
      const regs = mutationRegs.get(n);
      if (!regs) continue;
      for (const { observer, options: o } of regs) {
        if (!self && !o.subtree) continue;
        if (type === "childList" && !o.childList) continue;
        if (type === "characterData" && !o.characterData) continue;
        if (type === "attributes" && (!o.attributes || (o.attributeFilter && !o.attributeFilter.includes(name)))) continue;
        (out || (out = new Map())).set(observer, o);
      }
    }
    return out;
  }
  function queueRecord(observers, record) {
    for (const [observer, o] of observers) {
      const keepOld = record.type === "attributes" ? o.attributeOldValue : o.characterDataOldValue;
      observer._records.push(keepOld ? record : { ...record, oldValue: null });
      pendingObservers.add(observer);
    }
    if (!deliveryQueued) {
      deliveryQueued = true;
      Promise.resolve().then(deliverMutations);
    }
  }
  function deliverMutations() {
    deliveryQueued = false;
    const observers = Array.from(pendingObservers);
    pendingObservers.clear();
    for (const observer of observers) {
      const records = observer.takeRecords();
      if (records.length) {
        try { observer._cb.call(observer, records, observer); } catch (e) { reportError(e); }
      }
    }
  }
  const siblingsOf = (parent, first, last) => {
    const kids = N.children(parent);
    const i = kids.indexOf(first), j = kids.indexOf(last);
    return [i > 0 ? kids[i - 1] : null, j >= 0 && j + 1 < kids.length ? kids[j + 1] : null];
  };
  const childListRecord = (target, added, removed, prev, next) => ({
    type: "childList", target, addedNodes: added, removedNodes: removed, previousSibling: prev, nextSibling: next,
    attributeName: null, attributeNamespace: null, oldValue: null,
  });
  {
    const insert = N.insert, remove = N.remove, setAttr = N.setAttr, removeAttr = N.removeAttr, setText = N.setText;
    N.insert = (p, c, b) => {
      if (!mutationRegs.size) return insert(p, c, b);
      const fragment = N.nodeType(c) === 11 && N.shadowHost(c) === null;
      const moving = fragment ? N.children(c) : [c];
      const from = fragment ? c : N.parent(c);
      const leaving = from !== null && from !== undefined && moving.length ? interested(from, "childList") : null;
      const leftAt = leaving ? siblingsOf(from, moving[0], moving[moving.length - 1]) : null;
      insert(p, c, b);
      if (leaving) queueRecord(leaving, childListRecord(from, [], moving, ...leftAt));
      const watchers = moving.length ? interested(p, "childList") : null;
      if (watchers) queueRecord(watchers, childListRecord(p, moving, [], ...siblingsOf(p, moving[0], moving[moving.length - 1])));
    };
    N.remove = (id) => {
      const p = mutationRegs.size ? N.parent(id) : null;
      const watchers = p !== null && p !== undefined ? interested(p, "childList") : null;
      const at = watchers ? siblingsOf(p, id, id) : null;
      remove(id);
      if (watchers) queueRecord(watchers, childListRecord(p, [], [id], ...at));
    };
    N.setAttr = (id, name, value) => {
      const watchers = mutationRegs.size ? interested(id, "attributes", String(name)) : null;
      const old = watchers ? N.getAttr(id, name) : null;
      setAttr(id, name, value);
      if (watchers) queueRecord(watchers, { type: "attributes", target: id, addedNodes: [], removedNodes: [], previousSibling: null, nextSibling: null, attributeName: String(name), attributeNamespace: null, oldValue: old });
    };
    N.removeAttr = (id, name) => {
      const watchers = mutationRegs.size && N.getAttr(id, name) !== null ? interested(id, "attributes", String(name)) : null;
      const old = watchers ? N.getAttr(id, name) : null;
      removeAttr(id, name);
      if (watchers) queueRecord(watchers, { type: "attributes", target: id, addedNodes: [], removedNodes: [], previousSibling: null, nextSibling: null, attributeName: String(name), attributeNamespace: null, oldValue: old });
    };
    N.setText = (id, text) => {
      if (!mutationRegs.size) return setText(id, text);
      const type = N.nodeType(id);
      if (type === 3 || type === 8) {
        const watchers = interested(id, "characterData");
        const old = watchers ? N.text(id) : null;
        setText(id, text);
        if (watchers) queueRecord(watchers, { type: "characterData", target: id, addedNodes: [], removedNodes: [], previousSibling: null, nextSibling: null, attributeName: null, attributeNamespace: null, oldValue: old });
        return;
      }
      const watchers = interested(id, "childList");
      const removed = watchers ? N.children(id) : [];
      setText(id, text);
      if (watchers) queueRecord(watchers, childListRecord(id, N.children(id), removed, null, null));
    };
  }
  class MutationObserver {
    constructor(cb) {
      if (typeof cb !== "function") throw new TypeError("MutationObserver needs a callback");
      this._cb = cb;
      this._records = [];
      this._targets = new Set();
    }
    observe(target, options = {}) {
      if (!target || target.__id === undefined) throw new TypeError("MutationObserver.observe: not a Node");
      const o = { ...options };
      if (o.attributeOldValue || o.attributeFilter) o.attributes = o.attributes !== false;
      if (o.characterDataOldValue) o.characterData = o.characterData !== false;
      if (!o.childList && !o.attributes && !o.characterData) throw new TypeError("MutationObserver.observe: nothing to observe");
      if (o.attributeFilter) o.attributeFilter = Array.from(o.attributeFilter, String);
      const regs = mutationRegs.get(target.__id) || [];
      const existing = regs.find((r) => r.observer === this);
      if (existing) existing.options = o; else regs.push({ observer: this, options: o });
      mutationRegs.set(target.__id, regs);
      this._targets.add(target.__id);
    }
    disconnect() {
      for (const id of this._targets) {
        const regs = (mutationRegs.get(id) || []).filter((r) => r.observer !== this);
        if (regs.length) mutationRegs.set(id, regs); else mutationRegs.delete(id);
      }
      this._targets.clear();
      this._records = [];
    }
    takeRecords() {
      const out = this._records.map((r) => ({
        ...r,
        target: wrap(r.target),
        addedNodes: nodeList(r.addedNodes.map(wrap)),
        removedNodes: nodeList(r.removedNodes.map(wrap)),
        previousSibling: r.previousSibling === null ? null : wrap(r.previousSibling),
        nextSibling: r.nextSibling === null ? null : wrap(r.nextSibling),
      }));
      this._records = [];
      return out;
    }
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
    // document.createEvent()-style setup.
    initEvent(type, bubbles, cancelable) { this.type = String(type); this.bubbles = !!bubbles; this.cancelable = !!cancelable; }
    get returnValue() { return !this.defaultPrevented; }
    set returnValue(v) { if (!v) this.preventDefault(); }
  }
  class CustomEvent extends Event {
    constructor(type, init) { super(type, init); this.detail = init && init.detail !== undefined ? init.detail : null; }
    initCustomEvent(type, bubbles, cancelable, detail) { this.initEvent(type, bubbles, cancelable); this.detail = detail === undefined ? null : detail; }
  }
  class UIEvent extends Event {}
  class MouseEvent extends UIEvent {
    constructor(type, init) {
      super(type, init); init = init || {};
      this.button = init.button || 0; this.buttons = init.buttons || 0;
      this.clientX = init.clientX || 0; this.clientY = init.clientY || 0;
      this.ctrlKey = !!init.ctrlKey; this.shiftKey = !!init.shiftKey; this.altKey = !!init.altKey; this.metaKey = !!init.metaKey;
    }
    initMouseEvent(type, bubbles, cancelable, view, detail, sx, sy, cx, cy, ctrl, alt, shift, meta, button, related) {
      this.initEvent(type, bubbles, cancelable);
      Object.assign(this, { view, detail, screenX: sx, screenY: sy, clientX: cx, clientY: cy, ctrlKey: !!ctrl, altKey: !!alt, shiftKey: !!shift, metaKey: !!meta, button: button || 0, relatedTarget: related || null });
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
      if (MOUSE_TYPES.includes(type)) { if (this.__id !== undefined) mouseListeners.add(this.__id); N.touch(); }
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
      ev._origin = this;
      const path = [];
      for (let n = this; n; n = parentForEvents(n)) path.push(n);
      ev._path = path;
      // `window.event` while it's being handled (legacy, still used).
      const outer = currentEvent;
      currentEvent = ev;
      try {
        // Capture, target, bubble.
        for (let i = path.length - 1; i > 0 && !ev._stop; i--) invoke(path[i], ev, 1, true);
        if (!ev._stop) invoke(path[0], ev, 2, null);
        if (ev.bubbles) for (let i = 1; i < path.length && !ev._stop; i++) invoke(path[i], ev, 3, false);
      } finally {
        currentEvent = outer;
      }
      ev.currentTarget = null;
      ev.eventPhase = 0;
      return !ev.defaultPrevented;
    }
  }
  let currentEvent;
  function parentForEvents(n) {
    if (n === window) return null;
    if (n === document) return window;
    if (n instanceof ShadowRoot) return n.host;
    // Slotted content's events go through the slot it's shown in.
    const slot = assignedSlotOf(n);
    if (slot) return slot;
    return n.parentNode;
  }
  // The <slot> in its parent's shadow tree that shows `n`, if any.
  function assignedSlotOf(n) {
    const host = n.parentNode;
    const root = host && host._shadow;
    if (!root) return null;
    const name = n.nodeType === 1 ? (n.getAttribute("slot") || "") : "";
    if (n.nodeType === 3 && !/\S/.test(n.data || "")) return null;
    for (const slot of root.querySelectorAll("slot")) {
      if ((slot.getAttribute("name") || "") === name) return slot;
    }
    return null;
  }
  // What `target` looks like from `node`: listeners outside a shadow tree
  // see its host, not what's inside.
  function retarget(target, node) {
    for (let t = target; t; ) {
      let root = t;
      while (root.parentNode) root = root.parentNode;
      if (!(root instanceof ShadowRoot)) return t;
      for (let x = node; x; x = x instanceof ShadowRoot ? x.host : x.parentNode) {
        if (x === root) return t;
      }
      t = root.host;
    }
    return target;
  }
  function invoke(node, ev, phase, capture) {
    ev.currentTarget = node;
    if (ev._origin && node !== window && node !== document) ev.target = retarget(ev._origin, node);
    else if (ev._origin) ev.target = retarget(ev._origin, document.documentElement);
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
    let text = String(e);
    try { if (e && e.stack && !String(e.stack).startsWith(text)) text += "\n" + e.stack; } catch (_) {}
    N.log("error", text);
  }
  const clickListeners = new Set();
  // Legacy keyCode for the keys pages still check that way.
  const KEY_CODES = { Enter: 13, Escape: 27, Backspace: 8, Tab: 9, " ": 32, ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40, Delete: 46, Home: 36, End: 35, PageUp: 33, PageDown: 34, Shift: 16, Control: 17, Alt: 18, Meta: 91 };
  // The focused element (document.activeElement), shared with the tab: its
  // focus changes arrive through __setFocus, scripts' go to it through
  // N.setFocus.
  let focusedElement = null;
  // Focus moves to `el` (or nowhere): blur/focusout on the old element,
  // focus/focusin on the new one.
  function moveFocus(el) {
    const old = focusedElement;
    if (old === el) return;
    focusedElement = el;
    if (old) {
      old.dispatchEvent(new FocusEvent("blur", { relatedTarget: el }));
      old.dispatchEvent(new FocusEvent("focusout", { bubbles: true, composed: true, relatedTarget: el }));
    }
    if (el) {
      el.dispatchEvent(new FocusEvent("focus", { relatedTarget: old }));
      el.dispatchEvent(new FocusEvent("focusin", { bubbles: true, composed: true, relatedTarget: old }));
    }
  }
  const mouseListeners = new Set();
  const MOUSE_TYPES = ["mousedown", "mouseup", "mousemove", "dblclick", "wheel", "pointerdown", "pointerup", "pointermove", "contextmenu",
    "mouseover", "mouseout", "mouseenter", "mouseleave", "pointerover", "pointerout", "pointerenter", "pointerleave"];

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
    else if (type === 11) proto = N.shadowHost(id) !== null ? ShadowRoot.prototype : DocumentFragment.prototype;
    else proto = Node.prototype;
    w = Object.create(proto);
    Object.defineProperty(w, "__id", { value: id });
    wrappers.set(id, w);
    if (type === 1 && registry.size && !creatingInert && registry.has(N.tag(id)) && !inert(id)) upgrade(w);
    return w;
  }
  // Template contents are inert: custom elements there aren't upgraded
  // (frameworks read their binding attributes before stamping copies).
  const inertFragments = new Set();
  let creatingInert = false;
  function inert(id) {
    for (let n = N.parent(id); n !== null && n !== undefined; n = N.parent(n)) {
      const t = N.nodeType(n);
      if (t === 11) return inertFragments.has(n);
      if (t === 1 && N.tag(n) === "template") return true;
    }
    return false;
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
    // Merge adjacent text nodes, drop empty ones.
    normalize() {
      for (let c = this.firstChild; c; ) {
        const next = c.nextSibling;
        if (c.nodeType === 3) {
          while (c.nextSibling && c.nextSibling.nodeType === 3) { c.data += c.nextSibling.data; c.nextSibling.remove(); }
          if (!c.data) c.remove();
        } else if (c.nodeType === 1) c.normalize();
        c = c.parentNode === this ? c.nextSibling : next;
      }
    }
    get assignedSlot() { return assignedSlotOf(this); }
    // On a <slot>: the light nodes shown in it.
    assignedNodes(opts) {
      if (this.localName !== "slot") return [];
      const root = this.getRootNode();
      if (!(root instanceof ShadowRoot)) return [];
      const shown = Array.from(root.host.childNodes).filter((n) => assignedSlotOf(n) === this);
      if (shown.length || !(opts && opts.flatten)) return shown;
      return Array.from(this.childNodes);
    }
    assignedElements(opts) { return this.assignedNodes(opts).filter((n) => n.nodeType === 1); }
    get nodeType() { return N.nodeType(this.__id); }
    get nodeName() { const t = N.tag(this.__id); return t ? t.toUpperCase() : (this.nodeType === 3 ? "#text" : this.nodeType === 9 ? "#document" : "#node"); }
    get parentNode() { return wrap(N.parent(this.__id)); }
    get parentElement() { const p = this.parentNode; return p && p.nodeType === 1 ? p : null; }
    get childNodes() { return nodeList(ids(N.children(this.__id))); }
    get firstChild() { const c = N.children(this.__id); return wrap(c.length ? c[0] : null); }
    get lastChild() { const c = N.children(this.__id); return wrap(c.length ? c[c.length - 1] : null); }
    get nextSibling() { return sibling(this, 1, false); }
    get previousSibling() { return sibling(this, -1, false); }
    get ownerDocument() {
      // Template contents belong to an inert document of their own.
      const id = this.__id;
      // A document has none (jQuery climbs `parentNode || ownerDocument`).
      if (N.nodeType(id) === 9) return null;
      return inertFragments.has(id) || inert(id) ? inertDocument : document;
    }
    get isConnected() { return N.connected(this.__id); }
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
  // Node types pages (and polyfills patching them) expect to exist.
  class CDATASection extends Text {}
  class ProcessingInstruction extends CharacterData {}
  class DocumentType extends Node {}
  class Attr extends Node {}

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
  // [[name, value (with " !important")], ...], tokenized as CSS (natively).
  function parseDecls(text) {
    return N.cssDecls(String(text || "")).map(([name, value, important]) => [name, important ? value + " !important" : value]);
  }
  function styleFor(el) {
    const decls = () => parseDecls(el.getAttribute("style"));
    const write = (list) => {
      const text = list.map(([k, v]) => k + ": " + v).join("; ");
      if (text) el.setAttribute("style", text); else el.removeAttribute("style");
    };
    const api = {
      // Values read back serialized as browsers do (`.5` → `0.5`).
      getPropertyValue(name) { const d = decls().find(([k]) => k === name); return d ? d[1].replace(/\s*!important$/, "").replace(/(^|[\s(,])(-?)\.(\d)/g, "$1$20.$3") : ""; },
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
    get namespaceURI() { return N.namespace(this.__id); }
    getAttribute(name) { return N.getAttr(this.__id, String(name)); }
    setAttribute(name, value) { N.setAttr(this.__id, String(name), String(value)); }
    removeAttribute(name) { N.removeAttr(this.__id, String(name)); }
    hasAttribute(name) { return N.getAttr(this.__id, String(name)) !== null; }
    hasAttributes() { return N.attrNames(this.__id).length > 0; }
    getAttributeNode(name) { const value = this.getAttribute(name); return value === null ? null : { name, localName: name, value, ownerElement: this, specified: true }; }
    lookupNamespaceURI() { return null; }
    getAnimations() { return []; }
    requestFullscreen() { return Promise.reject(new TypeError("Fullscreen isn't available")); }
    setPointerCapture() {} releasePointerCapture() {} hasPointerCapture() { return false; }
    toggleAttribute(name, force) {
      const has = this.hasAttribute(name);
      const want = force === undefined ? !has : !!force;
      if (want && !has) this.setAttribute(name, ""); else if (!want && has) this.removeAttribute(name);
      return want;
    }
    getAttributeNames() { return N.attrNames(this.__id); }
    get attributes() {
      // A NamedNodeMap: indexed, and by name (`attributes.class`).
      const el = this;
      const list = N.attrNames(this.__id).map((name) => ({
        name, localName: name, namespaceURI: null, prefix: null, specified: true, ownerElement: el,
        get value() { return el.getAttribute(name) ?? ""; },
        set value(v) { el.setAttribute(name, v); },
        get nodeValue() { return this.value; },
        nodeType: 2, nodeName: name,
      }));
      for (const a of list) if (!(a.name in list) && !/^\d+$/.test(a.name)) Object.defineProperty(list, a.name, { value: a, enumerable: false });
      Object.defineProperties(list, {
        item: { value: (i) => list[i] || null },
        getNamedItem: { value: (n) => list.find((a) => a.name === String(n).toLowerCase()) || null },
        getNamedItemNS: { value: (_, n) => list.find((a) => a.name === n) || null },
        setNamedItem: { value: (a) => { el.setAttribute(a.name, a.value); return null; } },
        removeNamedItem: { value: (n) => { const a = list.getNamedItem(n); el.removeAttribute(n); return a; } },
      });
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
      N.insert(this.__id, N.parseHTML(this.localName, String(html), N.namespace(this.__id)), null);
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
    getBoundingClientRect() { const r = measure(this); return r ? domRect(r[0], r[1], r[2], r[3]) : domRect(0, 0, estimatedWidth(this), 0); }
    getClientRects() { const r = measure(this); return r ? [domRect(r[0], r[1], r[2], r[3])] : []; }
    scrollIntoView() {}
    scrollTo() {}
    scroll() {}
    focus() { if (focusedElement !== this) { moveFocus(this); N.setFocus(this.__id); } }
    blur() { if (focusedElement === this) { moveFocus(null); N.setFocus(null); } }
    click() { this.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })); }
    attachShadow(init) {
      // A declarative shadow root is handed over, emptied, the first time.
      if (this._shadow && this._shadow._declarative) {
        const root = this._shadow;
        root._declarative = false;
        root._mode = (init && init.mode) || "open";
        while (root.firstChild) root.removeChild(root.firstChild);
        return root;
      }
      if (this._shadow) throw new DOMException("Shadow root cannot be created on a host which already hosts a shadow tree.", "NotSupportedError");
      const root = wrap(N.attachShadow(this.__id));
      root._mode = (init && init.mode) || "open";
      this._shadow = root;
      shadowRoots.add(root);
      return root;
    }
    get shadowRoot() { return this._shadow && this._shadow._mode === "open" ? this._shadow : null; }
    // Popovers: shown by a marker attribute the UA stylesheet knows.
    showPopover() { this.setAttribute("data-feather-popover-open", ""); this.dispatchEvent(new Event("toggle")); }
    hidePopover() { this.removeAttribute("data-feather-popover-open"); this.dispatchEvent(new Event("toggle")); }
    togglePopover(force) {
      const open = force === undefined ? !this.hasAttribute("data-feather-popover-open") : !!force;
      if (open) this.showPopover(); else this.hidePopover();
      return open;
    }
    get popover() { return this.getAttribute("popover"); }
    set popover(v) { if (v === null) this.removeAttribute("popover"); else this.setAttribute("popover", v); }
    attachInternals() { return { setFormValue() {}, setValidity() {}, checkValidity() { return true; }, states: new Set(), form: null, labels: [], validity: { valid: true } }; }
    animate() { return { finished: Promise.resolve(), cancel() {}, play() {}, pause() {}, addEventListener() {} }; }
    // Geometry from the tab's layout (once it has reported this element;
    // the root element is the viewport).
    get clientWidth() { return this === document.documentElement ? viewport[0] : Math.round(measure(this)?.[2] || rootSize(this, 0)); }
    get clientHeight() { return this === document.documentElement ? viewport[1] : Math.round(measure(this)?.[3] || 0); }
    get offsetWidth() { return Math.round(measure(this)?.[2] || rootSize(this, 0)); }
    get offsetHeight() { return Math.round(measure(this)?.[3] || 0); }
    get offsetTop() { const r = measure(this); return r ? Math.round(r[1] + pageScroll()) : 0; }
    get offsetLeft() { return Math.round(measure(this)?.[0] || 0); }
    // The body (no positions known here), but never the body's or root's
    // own: `while (el = el.offsetParent)` loops have to end.
    get offsetParent() {
      if (this === document.body || this === document.documentElement || !N.connected(this.__id)) return null;
      return document.body;
    }
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
    // `new MyElement()` / `createElement` for a defined tag, or an upgrade
    // of an element that already exists (then: that element).
    constructor() {
      super();
      if (upgrading) {
        const el = upgrading;
        upgrading = null;
        return el;
      }
      const tag = tagOf.get(new.target);
      if (!tag) throw new TypeError("Illegal constructor");
      const id = N.createElement(tag);
      Object.defineProperty(this, "__id", { value: id });
      wrappers.set(id, this);
      upgraded.add(this);
    }
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
  for (const ev of ["click", "input", "change", "submit", "load", "error", "keydown", "keyup", "focus", "blur", "mouseover", "mouseout", "mouseenter", "mouseleave", "mousemove", "mousedown", "mouseup", "scroll", "resize", "abort", "loadstart", "loadend", "progress", "timeout", "readystatechange", "message", "open", "close", "popstate", "hashchange", "visibilitychange"]) {
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
  // Image loads: fetched natively (warming the page's image cache), then
  // `load` or `error` with the real size.
  const imageLoads = new Map();
  let imageLoadSeq = 0;
  function startImageLoad(img) {
    const src = img.getAttribute("src");
    if (!src) { img._img = null; return; }
    if (img._img && img._img.src === src) return;
    const id = ++imageLoadSeq;
    img._img = { src, state: "loading", w: 0, h: 0, id };
    imageLoads.set(id, img);
    N.imageLoad(id, src);
  }
  // A <link> put in the document: what it points at loads, then `load`
  // (vite waits for that before importing a chunk with CSS).
  function linkInserted(id) {
    const el = wrap(id);
    const rel = (el.getAttribute("rel") || "").toLowerCase();
    const href = el.getAttribute("href");
    if (!href || el._img || !/(^|\s)(stylesheet|preload|modulepreload|prefetch|icon)(\s|$)/.test(rel)) return;
    const loadId = ++imageLoadSeq;
    el._img = { src: href, state: "loading", w: 0, h: 0, id: loadId };
    imageLoads.set(loadId, el);
    N.resourceLoad(loadId, href);
  }
  function imageEvent(id, ok, w, h) {
    const img = imageLoads.get(id);
    imageLoads.delete(id);
    if (!img || !img._img || img._img.id !== id) return;
    Object.assign(img._img, { state: ok ? "done" : "broken", w, h });
    const waiters = img._decode || [];
    img._decode = null;
    for (const [resolve, reject] of waiters) ok ? resolve() : reject(new DOMException("The source image cannot be decoded.", "EncodingError"));
    img.dispatchEvent(new Event(ok ? "load" : "error"));
  }
  class HTMLImageElement extends HTMLElement {
    get complete() { return !this._img || this._img.state !== "loading"; }
    get naturalWidth() { return this._img && this._img.state === "done" ? this._img.w : 0; }
    get naturalHeight() { return this._img && this._img.state === "done" ? this._img.h : 0; }
    get currentSrc() { return this._img ? N.resolve(this._img.src) || "" : ""; }
    decode() {
      if (this.complete) return this._img && this._img.state === "broken" ? Promise.reject(new DOMException("The source image cannot be decoded.", "EncodingError")) : Promise.resolve();
      return new Promise((resolve, reject) => (this._decode || (this._decode = [])).push([resolve, reject]));
    }
    setAttribute(name, value) {
      super.setAttribute(name, value);
      if (String(name).toLowerCase() === "src") startImageLoad(this);
    }
    addEventListener(type, fn, opts) {
      super.addEventListener(type, fn, opts);
      if ((type === "load" || type === "error") && this._img === undefined) startImageLoad(this);
    }
    set onload(fn) { this._onload = fn; if (this._img === undefined) startImageLoad(this); }
    get onload() { return this._onload || null; }
    set onerror(fn) { this._onerror = fn; if (this._img === undefined) startImageLoad(this); }
    get onerror() { return this._onerror || null; }
  }
  reflect(HTMLImageElement, [["src", "src", "url"], ["alt", "alt"], ["srcset", "srcset"], ["sizes", "sizes"], ["loading", "loading"], ["decoding", "decoding"], ["crossOrigin", "crossorigin"]]);
  for (const dim of ["width", "height"]) {
    Object.defineProperty(HTMLImageElement.prototype, dim, {
      configurable: true,
      get() { const a = this.getAttribute(dim); return a !== null ? Number(a) || 0 : (dim === "width" ? this.naturalWidth : this.naturalHeight); },
      set(v) { this.setAttribute(dim, String(Math.floor(Number(v)) || 0)); },
    });
  }
  function Image(width, height) {
    const img = document.createElement("img");
    if (width !== undefined) img.width = width;
    if (height !== undefined) img.height = height;
    return img;
  }
  Image.prototype = HTMLImageElement.prototype;
  class HTMLScriptElement extends HTMLElement {
    get text() { return this.textContent; }
    set text(v) { this.textContent = v; }
  }
  reflect(HTMLScriptElement, [["src", "src", "url"], ["type", "type"], ["async", "async", "bool"], ["defer", "defer", "bool"], ["crossOrigin", "crossorigin"], ["noModule", "nomodule", "bool"]]);
  class HTMLLinkElement extends HTMLElement {}
  reflect(HTMLLinkElement, [["href", "href", "url"], ["rel", "rel"], ["as", "as"], ["media", "media"], ["type", "type"]]);
  class HTMLStyleElement extends HTMLElement {
    // Its stylesheet: the element's text until scripts edit the rules,
    // then those (until the text itself changes again).
    get sheet() {
      let sheet = this._sheet;
      if (!sheet) { sheet = this._sheet = new CSSStyleSheet(); sheet.ownerNode = this; }
      const text = this.textContent;
      if (!sheet._cssom || sheet._source !== text) { sheet._cssom = false; sheet._text = text; }
      return sheet;
    }
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
    submit() { N.submitForm(this.__id); }
    requestSubmit() { if (this.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }))) this.submit(); }
    reset() {}
  }
  reflect(HTMLFormElement, [["action", "action", "url"], ["method", "method"], ["name", "name"], ["target", "target"]]);
  class HTMLTemplateElement extends HTMLElement {
    get content() {
      // Parsed templates keep their contents in a fragment of their own.
      const parsed = N.templateContent(this.__id);
      if (parsed !== null) { inertFragments.add(parsed); return wrap(parsed); }
      // A script-made template: one fragment for good, holding what
      // innerHTML/appendChild put in the element.
      if (!this._content) { this._content = document.createDocumentFragment(); inertFragments.add(this._content.__id); }
      while (this.firstChild) this._content.appendChild(this.firstChild);
      return this._content;
    }
    cloneNode(deep) {
      const copy = super.cloneNode(deep);
      // A script-made template's contents are ours to copy.
      if (deep && this._content) {
        copy._content = this._content.cloneNode(true);
        inertFragments.add(copy._content.__id);
      }
      return copy;
    }
  }
  // Canvas 2D: the context records operations; before each snapshot they're
  // replayed natively (tiny-skia) and the canvas shows the result.
  const dirtyCanvases = new Set();
  function context2d(canvas) {
    const ops = [];
    const rec = (...op) => { ops.push(op); if (!dirtyCanvases.has(canvas)) { dirtyCanvases.add(canvas); N.touch(); } };
    const props = {
      fillStyle: "#000000", strokeStyle: "#000000", lineWidth: 1, lineCap: "butt", lineJoin: "miter", miterLimit: 10,
      globalAlpha: 1, globalCompositeOperation: "source-over", lineDashOffset: 0,
      font: "10px sans-serif", textAlign: "start", textBaseline: "alphabetic", direction: "ltr",
      shadowBlur: 0, shadowColor: "rgba(0, 0, 0, 0)", shadowOffsetX: 0, shadowOffsetY: 0,
      imageSmoothingEnabled: true, imageSmoothingQuality: "low", filter: "none",
    };
    const recorded = new Set(["fillStyle", "strokeStyle", "lineWidth", "lineCap", "lineJoin", "miterLimit", "globalAlpha", "globalCompositeOperation", "lineDashOffset"]);
    const style = (v) => (v && v._gradient ? v._gradient : String(v));
    let stack = [];
    let matrix = [1, 0, 0, 1, 0, 0];
    let dash = [];
    const mul = (m, n) => [m[0] * n[0] + m[2] * n[1], m[1] * n[0] + m[3] * n[1], m[0] * n[2] + m[2] * n[3], m[1] * n[2] + m[3] * n[3], m[0] * n[4] + m[2] * n[5] + m[4], m[1] * n[4] + m[3] * n[5] + m[5]];
    const gradient = (g) => ({
      _gradient: g,
      addColorStop(offset, color) { g[g.length - 1].push([Number(offset), String(color)]); },
    });
    const ctx = {
      canvas,
      _ops: ops,
      save() { stack.push([Object.assign({}, props), matrix.slice(), dash.slice()]); rec("save"); },
      restore() { const s = stack.pop(); if (s) { Object.assign(props, s[0]); matrix = s[1]; dash = s[2]; } rec("restore"); },
      scale(x, y) { matrix = mul(matrix, [x, 0, 0, y, 0, 0]); rec("scale", x, y); },
      rotate(a) { const c = Math.cos(a), s = Math.sin(a); matrix = mul(matrix, [c, s, -s, c, 0, 0]); rec("rotate", a); },
      translate(x, y) { matrix = mul(matrix, [1, 0, 0, 1, x, y]); rec("translate", x, y); },
      transform(a, b, c, d, e, f) { matrix = mul(matrix, [a, b, c, d, e, f]); rec("transform", a, b, c, d, e, f); },
      setTransform(a, b, c, d, e, f) {
        if (a && typeof a === "object") ({ a, b, c, d, e, f } = a);
        if (a === undefined) [a, b, c, d, e, f] = [1, 0, 0, 1, 0, 0];
        matrix = [a, b, c, d, e, f];
        rec("setTransform", a, b, c, d, e, f);
      },
      resetTransform() { this.setTransform(1, 0, 0, 1, 0, 0); },
      getTransform() { const [a, b, c, d, e, f] = matrix; return { a, b, c, d, e, f, m11: a, m12: b, m21: c, m22: d, m41: e, m42: f, is2D: true, isIdentity: a === 1 && b === 0 && c === 0 && d === 1 && e === 0 && f === 0 }; },
      beginPath() { rec("beginPath"); }, closePath() { rec("closePath"); },
      moveTo(x, y) { rec("moveTo", x, y); }, lineTo(x, y) { rec("lineTo", x, y); },
      quadraticCurveTo(a, b, c, d) { rec("quadraticCurveTo", a, b, c, d); },
      bezierCurveTo(a, b, c, d, e, f) { rec("bezierCurveTo", a, b, c, d, e, f); },
      arc(x, y, r, a0, a1, ccw) { rec("arc", x, y, r, a0, a1, ccw ? 1 : 0); },
      arcTo(x1, y1, x2, y2) { rec("lineTo", x1, y1); rec("lineTo", x2, y2); },
      ellipse(x, y, rx, ry, rot, a0, a1, ccw) { rec("ellipse", x, y, rx, ry, rot, a0, a1, ccw ? 1 : 0); },
      rect(x, y, w, h) { rec("rect", x, y, w, h); },
      roundRect(x, y, w, h, r) { rec("roundRect", x, y, w, h, Array.isArray(r) ? r[0] || 0 : r || 0); },
      fill(a, b) {
        const rule = typeof a === "string" ? a : typeof b === "string" ? b : "nonzero";
        if (a instanceof Path2D) { rec("beginPath"); for (const [op, args] of a._ops) rec(op, ...args); }
        rec("fill", rule);
      },
      stroke(p) {
        if (p instanceof Path2D) { rec("beginPath"); for (const [op, args] of p._ops) rec(op, ...args); }
        rec("stroke");
      },
      clip(p) {
        if (p instanceof Path2D) { rec("beginPath"); for (const [op, args] of p._ops) rec(op, ...args); }
        rec("clip");
      },
      fillRect(x, y, w, h) { rec("fillRect", x, y, w, h); },
      strokeRect(x, y, w, h) { rec("strokeRect", x, y, w, h); },
      clearRect(x, y, w, h) { rec("clearRect", x, y, w, h); },
      isPointInPath() { return false; }, isPointInStroke() { return false; },
      setLineDash(d) { dash = Array.from(d || [], Number); rec("setLineDash", dash); },
      getLineDash() { return dash.slice(); },
      fillText() {}, strokeText() {},
      measureText(text) {
        const size = parseFloat(/(\d+(?:\.\d+)?)px/.exec(props.font)?.[1] || "10");
        const width = String(text).length * size * 0.55;
        return { width, actualBoundingBoxAscent: size * 0.8, actualBoundingBoxDescent: size * 0.2, actualBoundingBoxLeft: 0, actualBoundingBoxRight: width, fontBoundingBoxAscent: size * 0.8, fontBoundingBoxDescent: size * 0.2 };
      },
      drawImage(img, ...args) {
        if (!img) return;
        if (img instanceof HTMLCanvasElement) return rec("drawCanvas", img.__id, ...args);
        const src = img.currentSrc || (img.getAttribute && N.resolve(img.getAttribute("src") || "")) || img.src;
        if (src) rec("drawImage", String(src), ...args);
      },
      createLinearGradient: (x0, y0, x1, y1) => gradient(["linear", x0, y0, x1, y1, []]),
      createRadialGradient: (x0, y0, r0, x1, y1, r1) => gradient(["radial", x0, y0, r0, x1, y1, r1, []]),
      createConicGradient: (a, x, y) => gradient(["radial", x, y, 0, x, y, 1, []]),
      createPattern: () => null,
      createImageData: (w, h) => ({ width: w, height: h, data: new Uint8ClampedArray(Math.max(0, w * h * 4)) }),
      getImageData: (x, y, w, h) => ({ width: w, height: h, data: new Uint8ClampedArray(Math.max(0, w * h * 4)) }),
      putImageData() {},
      getContextAttributes: () => ({ alpha: true, desynchronized: false, colorSpace: "srgb", willReadFrequently: false }),
    };
    for (const name of Object.keys(props)) {
      Object.defineProperty(ctx, name, {
        enumerable: true,
        get() { return props[name]; },
        set(v) {
          props[name] = v;
          if (recorded.has(name)) rec(name, name === "fillStyle" || name === "strokeStyle" ? style(v) : v);
        },
      });
    }
    return ctx;
  }
  // [[canvas id, width, height, ops], ...] for canvases drawn on since the
  // last snapshot.
  function flushCanvases() {
    const out = [];
    for (const c of dirtyCanvases) {
      const ctx = c._ctx;
      if (!ctx) continue;
      out.push([c.__id, c.width, c.height, ctx._ops.splice(0)]);
    }
    dirtyCanvases.clear();
    return out;
  }
  class HTMLCanvasElement extends HTMLElement {
    getContext(kind) {
      if (kind !== "2d") return null;
      return this._ctx || (this._ctx = context2d(this));
    }
    get width() { const a = this.getAttribute("width"); return a === null ? 300 : Number(a) || 0; }
    // Setting the size clears the canvas (and its context's state).
    set width(v) { this.setAttribute("width", String(Math.floor(Number(v)) || 0)); if (this._ctx) { this._ctx._ops.length = 0; this._ctx.setTransform(1, 0, 0, 1, 0, 0); } }
    get height() { const a = this.getAttribute("height"); return a === null ? 150 : Number(a) || 0; }
    set height(v) { this.setAttribute("height", String(Math.floor(Number(v)) || 0)); if (this._ctx) { this._ctx._ops.length = 0; this._ctx.setTransform(1, 0, 0, 1, 0, 0); } }
    toDataURL() { return "data:,"; }
    toBlob(cb) { setTimeout(() => cb(new Blob([], { type: "image/png" })), 0); }
  }
  // <video>/<audio>: the whole API, in the state of a media element
  // with nothing it can play (there's no playback yet). Players read and
  // listen to all of it; a missing `textTracks` threw in the Guardian's,
  // and took the rest of its page's effects down with it.
  class TimeRanges {
    get length() { return 0; }
    start() { throw new DOMException("Index out of range", "IndexSizeError"); }
    end() { throw new DOMException("Index out of range", "IndexSizeError"); }
  }
  class TextTrack extends EventTarget {
    constructor(kind = "", label = "", language = "") {
      super();
      Object.assign(this, { kind, label, language, id: "", mode: "disabled", inBandMetadataTrackDispatchType: "" });
      this.cues = []; this.activeCues = [];
      this.oncuechange = null;
    }
    addCue(cue) { this.cues.push(cue); }
    removeCue(cue) { const i = this.cues.indexOf(cue); if (i >= 0) this.cues.splice(i, 1); }
  }
  class MediaTrackList extends EventTarget {
    constructor() { super(); this._tracks = []; this.onaddtrack = null; this.onremovetrack = null; this.onchange = null; }
    get length() { return this._tracks.length; }
    item(i) { return this._tracks[i] || null; }
    getTrackById(id) { return this._tracks.find((t) => t.id === id) || null; }
    [Symbol.iterator]() { return this._tracks[Symbol.iterator](); }
    _add(track) {
      const i = this._tracks.length;
      this._tracks.push(track);
      Object.defineProperty(this, i, { get: () => this._tracks[i], configurable: true });
      const ev = new Event("addtrack"); ev.track = track;
      this.dispatchEvent(ev);
    }
  }
  class HTMLMediaElement extends HTMLElement {
    _media() {
      return this.__media || (this.__media = {
        textTracks: new MediaTrackList(), audioTracks: new MediaTrackList(), videoTracks: new MediaTrackList(),
        currentTime: 0, volume: 1, muted: null, playbackRate: 1, defaultPlaybackRate: 1,
      });
    }
    get textTracks() { return this._media().textTracks; }
    get audioTracks() { return this._media().audioTracks; }
    get videoTracks() { return this._media().videoTracks; }
    addTextTrack(kind, label, language) {
      const t = new TextTrack(kind, label, language);
      this._media().textTracks._add(t);
      return t;
    }
    get buffered() { return new TimeRanges(); }
    get played() { return new TimeRanges(); }
    get seekable() { return new TimeRanges(); }
    get currentSrc() { return ""; }
    get currentTime() { return this._media().currentTime; }
    set currentTime(v) { this._media().currentTime = Number(v) || 0; }
    get duration() { return NaN; }
    get paused() { return true; }
    get ended() { return false; }
    get seeking() { return false; }
    get error() { return null; }
    get readyState() { return 0; }
    get networkState() { return this.getAttribute("src") || this.querySelector("source") ? 3 : 0; }
    get volume() { return this._media().volume; }
    set volume(v) {
      v = Number(v);
      if (!(v >= 0 && v <= 1)) throw new DOMException("Volume out of range", "IndexSizeError");
      this._media().volume = v;
      this.dispatchEvent(new Event("volumechange"));
    }
    get muted() { const m = this._media().muted; return m === null ? this.hasAttribute("muted") : m; }
    set muted(v) { this._media().muted = !!v; this.dispatchEvent(new Event("volumechange")); }
    get playbackRate() { return this._media().playbackRate; }
    set playbackRate(v) { this._media().playbackRate = Number(v); this.dispatchEvent(new Event("ratechange")); }
    get defaultPlaybackRate() { return this._media().defaultPlaybackRate; }
    set defaultPlaybackRate(v) { this._media().defaultPlaybackRate = Number(v); }
    get srcObject() { return this._media().srcObject || null; }
    set srcObject(v) { this._media().srcObject = v; }
    play() { return Promise.reject(new DOMException("The element has no supported sources.", "NotSupportedError")); }
    pause() {}
    load() {}
    fastSeek(t) { this.currentTime = t; }
    canPlayType() { return ""; }
    captureStream() { throw new DOMException("Not supported", "NotSupportedError"); }
    setMediaKeys() { return Promise.reject(new DOMException("Not supported", "NotSupportedError")); }
    get mediaKeys() { return null; }
  }
  Object.assign(HTMLMediaElement, { NETWORK_EMPTY: 0, NETWORK_IDLE: 1, NETWORK_LOADING: 2, NETWORK_NO_SOURCE: 3, HAVE_NOTHING: 0, HAVE_METADATA: 1, HAVE_CURRENT_DATA: 2, HAVE_FUTURE_DATA: 3, HAVE_ENOUGH_DATA: 4 });
  Object.assign(HTMLMediaElement.prototype, { NETWORK_EMPTY: 0, NETWORK_IDLE: 1, NETWORK_LOADING: 2, NETWORK_NO_SOURCE: 3, HAVE_NOTHING: 0, HAVE_METADATA: 1, HAVE_CURRENT_DATA: 2, HAVE_FUTURE_DATA: 3, HAVE_ENOUGH_DATA: 4 });
  reflect(HTMLMediaElement, [
    ["src", "src", "url"], ["preload", "preload"], ["autoplay", "autoplay", "bool"], ["loop", "loop", "bool"],
    ["controls", "controls", "bool"], ["defaultMuted", "muted", "bool"], ["crossOrigin", "crossorigin"],
  ]);
  class HTMLVideoElement extends HTMLMediaElement {
    get videoWidth() { return 0; }
    get videoHeight() { return 0; }
    getVideoPlaybackQuality() { return { creationTime: N.now(), droppedVideoFrames: 0, totalVideoFrames: 0, corruptedVideoFrames: 0 }; }
    requestVideoFrameCallback() { return 0; }
    cancelVideoFrameCallback() {}
    requestPictureInPicture() { return Promise.reject(new DOMException("Not supported", "NotSupportedError")); }
  }
  reflect(HTMLVideoElement, [["poster", "poster", "url"], ["playsInline", "playsinline", "bool"], ["width", "width", "num"], ["height", "height", "num"]]);
  class HTMLAudioElement extends HTMLMediaElement {}
  class HTMLIFrameElement extends HTMLElement {
    get contentWindow() { return null; }
    get contentDocument() { return null; }
  }
  // Element interfaces with nothing of their own here, for `instanceof`.
  const plain = (name, base = HTMLElement) => ({ [name]: class extends base {} })[name];
  const HTMLDivElement = plain("HTMLDivElement"), HTMLSpanElement = plain("HTMLSpanElement");
  const HTMLParagraphElement = plain("HTMLParagraphElement"), HTMLHeadingElement = plain("HTMLHeadingElement");
  const HTMLUListElement = plain("HTMLUListElement"), HTMLOListElement = plain("HTMLOListElement"), HTMLLIElement = plain("HTMLLIElement");
  const HTMLTableElement = plain("HTMLTableElement"), HTMLTableRowElement = plain("HTMLTableRowElement"), HTMLTableCellElement = plain("HTMLTableCellElement");
  const HTMLLabelElement = plain("HTMLLabelElement"), HTMLPictureElement = plain("HTMLPictureElement"), HTMLSourceElement = plain("HTMLSourceElement");
  const HTMLSlotElement = plain("HTMLSlotElement"), HTMLUnknownElement = plain("HTMLUnknownElement"), HTMLBodyElement = plain("HTMLBodyElement");
  const HTMLHeadElement = plain("HTMLHeadElement"), HTMLHtmlElement = plain("HTMLHtmlElement"), HTMLMetaElement = plain("HTMLMetaElement");
  const HTMLBRElement = plain("HTMLBRElement"), HTMLHRElement = plain("HTMLHRElement"), HTMLPreElement = plain("HTMLPreElement");
  const HTMLDialogElement = plain("HTMLDialogElement"), HTMLDetailsElement = plain("HTMLDetailsElement");
  // More interfaces pages test with `instanceof`, by tag.
  const moreElements = {
    title: "HTMLTitleElement", fieldset: "HTMLFieldSetElement", legend: "HTMLLegendElement", progress: "HTMLProgressElement",
    output: "HTMLOutputElement", datalist: "HTMLDataListElement", object: "HTMLObjectElement", embed: "HTMLEmbedElement",
    area: "HTMLAreaElement", map: "HTMLMapElement", track: "HTMLTrackElement", blockquote: "HTMLQuoteElement", q: "HTMLQuoteElement",
    ins: "HTMLModElement", del: "HTMLModElement", time: "HTMLTimeElement", data: "HTMLDataElement", meter: "HTMLMeterElement",
    menu: "HTMLMenuElement", caption: "HTMLTableCaptionElement", thead: "HTMLTableSectionElement", tbody: "HTMLTableSectionElement",
    tfoot: "HTMLTableSectionElement", col: "HTMLTableColElement", colgroup: "HTMLTableColElement", nav: null,
  };
  const moreElementClasses = {};
  for (const [tag, name] of Object.entries(moreElements)) {
    if (!name) continue;
    moreElementClasses[tag] = moreElementClasses[name] || (moreElementClasses[name] = plain(name));
  }
  const SVGElement = plain("SVGElement", Element);
  // SVG elements have style, dataset, focus()... too (HTMLOrSVGElement).
  for (const key of Reflect.ownKeys(HTMLElement.prototype)) {
    if (key === "constructor" || key in Element.prototype) continue;
    Object.defineProperty(SVGElement.prototype, key, Object.getOwnPropertyDescriptor(HTMLElement.prototype, key));
  }
  const SVGSVGElement = plain("SVGSVGElement", SVGElement), SVGPathElement = plain("SVGPathElement", SVGElement), SVGGElement = plain("SVGGElement", SVGElement);
  const elementClasses = {
    a: HTMLAnchorElement, img: HTMLImageElement, script: HTMLScriptElement, link: HTMLLinkElement, style: HTMLStyleElement,
    input: HTMLInputElement, textarea: HTMLTextAreaElement, select: HTMLSelectElement, option: HTMLOptionElement,
    button: HTMLButtonElement, form: HTMLFormElement, template: HTMLTemplateElement, canvas: HTMLCanvasElement,
    video: HTMLVideoElement, audio: HTMLAudioElement, iframe: HTMLIFrameElement,
    div: HTMLDivElement, span: HTMLSpanElement, p: HTMLParagraphElement,
    h1: HTMLHeadingElement, h2: HTMLHeadingElement, h3: HTMLHeadingElement, h4: HTMLHeadingElement, h5: HTMLHeadingElement, h6: HTMLHeadingElement,
    ul: HTMLUListElement, ol: HTMLOListElement, li: HTMLLIElement, table: HTMLTableElement, tr: HTMLTableRowElement, td: HTMLTableCellElement, th: HTMLTableCellElement,
    label: HTMLLabelElement, picture: HTMLPictureElement, source: HTMLSourceElement, slot: HTMLSlotElement, body: HTMLBodyElement, head: HTMLHeadElement,
    html: HTMLHtmlElement, meta: HTMLMetaElement, br: HTMLBRElement, hr: HTMLHRElement, pre: HTMLPreElement, dialog: HTMLDialogElement, details: HTMLDetailsElement,
    svg: SVGSVGElement, path: SVGPathElement, g: SVGGElement,
  };
  for (const tag of Object.keys(moreElements)) if (moreElementClasses[tag]) elementClasses[tag] = moreElementClasses[tag];

  class DocumentFragment extends Node {
    getElementById(id) { return this.querySelector("#" + cssEscape(id)); }
  }
  mixin(DocumentFragment, ParentMixin);

  // ---- shadow DOM and custom elements -----------------------------------

  const shadowRoots = new Set();
  class ShadowRoot extends DocumentFragment {
    get host() { return wrap(N.shadowHost(this.__id)); }
    get mode() { return this._mode || "open"; }
    get innerHTML() { return N.innerHTML(this.__id); }
    set innerHTML(html) {
      for (const c of N.children(this.__id)) N.remove(c);
      N.insert(this.__id, N.parseHTML("div", String(html)), null);
    }
    get adoptedStyleSheets() { return this._sheets || (this._sheets = []); }
    set adoptedStyleSheets(v) { this._sheets = Array.from(v); N.touch(); }
    get activeElement() { return null; }
    get delegatesFocus() { return false; }
    get clonable() { return false; }
    getSelection() { return document.getSelection(); }
    elementFromPoint() { return null; }
    elementsFromPoint() { return []; }
  }

  // Constructed stylesheets (adoptedStyleSheets): just their text.
  // Top-level rules of a stylesheet's text (tokenized natively).
  const splitRules = (text) => N.cssRules(String(text));
  class CSSRule {
    constructor(text, sheet) {
      this.cssText = text;
      this.parentStyleSheet = sheet;
      const brace = text.indexOf("{");
      this.selectorText = brace > 0 ? text.slice(0, brace).trim() : "";
      this.type = text.startsWith("@media") ? 4 : text.startsWith("@font-face") ? 5 : text.startsWith("@keyframes") ? 7 : text.startsWith("@import") ? 3 : 1;
      if (this.type === 4) this.conditionText = this.selectorText.slice(6).trim();
      if (this.type === 1) {
        const body = brace > 0 ? text.slice(brace + 1, text.lastIndexOf("}")) : "";
        const decls = parseDecls(body);
        this.style = {
          cssText: body.trim(),
          get length() { return decls.length; },
          item: (i) => (decls[i] ? decls[i][0] : ""),
          getPropertyValue: (n) => { const d = decls.find(([k]) => k === n); return d ? d[1].replace(/\s*!important$/, "") : ""; },
          getPropertyPriority: (n) => { const d = decls.find(([k]) => k === n); return d && / !important$/.test(d[1]) ? "important" : ""; },
        };
      }
    }
  }
  // Stylesheets as rule lists: constructed ones (adoptedStyleSheets) and
  // <style> elements' (CSS-in-JS libraries insertRule into those).
  class CSSStyleSheet {
    constructor() { this._rules = []; this.disabled = false; this.media = []; this.ownerNode = null; }
    get _text() { return this._rules.join("\n"); }
    set _text(v) { this._rules = splitRules(String(v)); }
    replaceSync(text) { this._text = String(text); this._changed(); }
    replace(text) { this.replaceSync(text); return Promise.resolve(this); }
    get cssRules() { return this._rules.map((r) => new CSSRule(r, this)); }
    get rules() { return this.cssRules; }
    get type() { return "text/css"; }
    insertRule(rule, index) {
      index = index === undefined ? 0 : Number(index);
      if (!(index >= 0 && index <= this._rules.length)) throw new DOMException("Index out of range", "IndexSizeError");
      this._rules.splice(index, 0, String(rule).trim());
      this._changed();
      return index;
    }
    deleteRule(index) {
      index = Number(index);
      if (!(index >= 0 && index < this._rules.length)) throw new DOMException("Index out of range", "IndexSizeError");
      this._rules.splice(index, 1);
      this._changed();
    }
    addRule(selector, style, index) { this.insertRule(selector + "{" + (style || "") + "}", index === undefined ? this._rules.length : index); return -1; }
    removeRule(index) { this.deleteRule(index || 0); }
    _changed() {
      if (this.ownerNode) { this._cssom = true; this._source = this.ownerNode.textContent; cssomStyles.add(this.ownerNode); }
      N.touch();
    }
  }
  const cssomStyles = new Set();

  let documentSheets = [];

  const registry = new Map();
  const tagOf = new Map();
  const upgraded = new WeakSet();
  const definedWaiters = new Map();
  let upgrading = null;
  const callback = (el, name, ...args) => {
    if (typeof el[name] === "function") { try { el[name](...args); } catch (e) { reportError(e); } }
  };
  function upgrade(el) {
    if (upgraded.has(el)) return;
    const def = registry.get(N.tag(el.__id));
    if (!def) return;
    upgraded.add(el);
    Object.setPrototypeOf(el, def.ctor.prototype);
    upgrading = el;
    try { Reflect.construct(def.ctor, []); } catch (e) { reportError(e); } finally { upgrading = null; }
    for (const name of def.observed) {
      const v = N.getAttr(el.__id, name);
      if (v !== null) callback(el, "attributeChangedCallback", name, null, v);
    }
    if (N.connected(el.__id)) callback(el, "connectedCallback");
  }
  // Custom elements in a subtree that just joined (or left) the document,
  // through their shadow trees too.
  function forEachCustom(id, fn) {
    const all = N.nodeType(id) === 1 ? [id, ...N.query(id, "*", true)] : N.query(id, "*", true);
    for (const n of all) {
      if (registry.has(N.tag(n))) fn(n);
      const w = wrappers.get(n);
      if (w && w._shadow) for (const c of N.children(w._shadow.__id)) forEachCustom(c, fn);
    }
  }
  function connected(id) {
    forEachCustom(id, (n) => {
      const known = wrappers.get(n);
      if (known && upgraded.has(known)) callback(known, "connectedCallback");
      else upgrade(wrap(n));
    });
  }
  function disconnected(id) {
    forEachCustom(id, (n) => { const w = wrappers.get(n); if (w && upgraded.has(w)) callback(w, "disconnectedCallback"); });
  }
  {
    const insert = N.insert, remove = N.remove, setAttr = N.setAttr, removeAttr = N.removeAttr;
    N.insert = (p, c, b) => {
      if (N.nodeType(c) === 1 && N.tag(c) === "link") {
        insert(p, c, b);
        if (N.connected(p)) linkInserted(c);
        return;
      }
      if (!registry.size) return insert(p, c, b);
      const moving = N.nodeType(c) === 11 && N.shadowHost(c) === null ? N.children(c) : [c];
      const before = moving.map((m) => N.connected(m));
      insert(p, c, b);
      if (!N.connected(p)) return;
      moving.forEach((m, i) => { if (!before[i]) connected(m); });
    };
    N.remove = (id) => {
      const was = registry.size && N.connected(id);
      remove(id);
      if (was) disconnected(id);
    };
    const observed = (id, name) => {
      const w = wrappers.get(id);
      if (!w || !upgraded.has(w)) return null;
      const def = registry.get(N.tag(id));
      return def && def.observed.includes(name) ? w : null;
    };
    N.setAttr = (id, name, value) => {
      const w = registry.size ? observed(id, String(name).toLowerCase()) : null;
      const old = w ? N.getAttr(id, name) : null;
      setAttr(id, name, value);
      if (w) callback(w, "attributeChangedCallback", String(name).toLowerCase(), old, String(value));
    };
    N.removeAttr = (id, name) => {
      const w = registry.size ? observed(id, String(name).toLowerCase()) : null;
      const old = w ? N.getAttr(id, name) : null;
      removeAttr(id, name);
      if (w && old !== null) callback(w, "attributeChangedCallback", String(name).toLowerCase(), old, null);
    };
  }
  const customElements = {
    define(name, ctor) {
      const tag = String(name).toLowerCase();
      if (registry.has(tag)) throw new DOMException("the name \"" + tag + "\" has already been used with this registry", "NotSupportedError");
      registry.set(tag, { ctor, observed: Array.from(ctor.observedAttributes || [], String) });
      tagOf.set(ctor, tag);
      // Upgrade the ones already there, in the document and shadow trees.
      const found = N.query(N.root(), tag, true);
      for (const r of shadowRoots) found.push(...N.query(r.__id, tag, true));
      for (const id of found) upgrade(wrap(id));
      const waiter = definedWaiters.get(tag);
      if (waiter) { waiter.resolve(ctor); definedWaiters.delete(tag); }
    },
    get(name) { const d = registry.get(String(name).toLowerCase()); return d ? d.ctor : undefined; },
    getName(ctor) { return tagOf.get(ctor) || null; },
    whenDefined(name) {
      const tag = String(name).toLowerCase();
      if (registry.has(tag)) return Promise.resolve(registry.get(tag).ctor);
      let w = definedWaiters.get(tag);
      if (!w) { let resolve; const promise = new Promise((r) => { resolve = r; }); w = { promise, resolve }; definedWaiters.set(tag, w); }
      return w.promise;
    },
    upgrade(root) { if (root && root.__id !== undefined) forEachCustom(root.__id, (n) => upgrade(wrap(n))); },
  };

  let readyState = "loading";
  let currentScript = null;
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
    getSelection() { return selection; }
    get fonts() { return fontFaceSet; }
    get styleSheets() { return Array.from(this.querySelectorAll("style")).map((el) => el.sheet); }
    get adoptedStyleSheets() { return documentSheets; }
    set adoptedStyleSheets(v) { documentSheets = Array.from(v); N.touch(); }
    get currentScript() { return currentScript; }
    get URL() { return N.url(); }
    get documentURI() { return N.url(); }
    get baseURI() { return N.url(); }
    get location() { return location; }
    set location(v) { location.href = v; }
    get defaultView() { return window; }
    get referrer() { return ""; }
    get domain() { return location.hostname; }
    get cookie() { return N.cookie(); }
    set cookie(v) { N.setCookie(String(v)); }
    get visibilityState() { return "visible"; }
    get hidden() { return false; }
    get compatMode() { return "CSS1Compat"; }
    get characterSet() { return "UTF-8"; }
    get contentType() { return "text/html"; }
    get activeElement() { return (focusedElement && focusedElement.isConnected !== false ? focusedElement : null) || this.body; }
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
    createElement(tag) {
      const def = registry.get(String(tag).toLowerCase());
      if (def) return new def.ctor();
      return wrap(N.createElement(String(tag)));
    }
    createElementNS(ns, tag) {
      const local = String(tag).replace(/^[^:]*:/, "");
      if (ns === "http://www.w3.org/1999/xhtml") return this.createElement(local);
      return wrap(N.createElement(local, ns));
    }
    createTextNode(text) { return wrap(N.createText(String(text))); }
    createComment(text) { return wrap(N.createComment(String(text))); }
    createDocumentFragment() { return wrap(N.createFragment()); }
    createEvent(kind) {
      const k = String(kind).toLowerCase();
      if (k === "customevent") return new CustomEvent("");
      if (k === "mouseevent" || k === "mouseevents") return new MouseEvent("");
      if (k === "keyboardevent" || k === "keyboardevents") return new KeyboardEvent("");
      if (k === "uievent" || k === "uievents") return new UIEvent("");
      if (k === "focusevent") return new FocusEvent("");
      return new Event("");
    }
    createRange() {
      return { selectNodeContents() {}, setStart() {}, setEnd() {}, collapse() {}, getBoundingClientRect() { return { top: 0, left: 0, width: 0, height: 0 }; }, createContextualFragment: (html) => wrap(N.parseHTML("div", String(html))) };
    }
    createTreeWalker(root, whatToShow, filter) { return new TreeWalker(root, whatToShow, filter); }
    createNodeIterator(root, whatToShow, filter) {
      const walker = new TreeWalker(root, whatToShow, filter);
      let started = false;
      return {
        root, whatToShow: walker.whatToShow, filter: walker.filter,
        nextNode() {
          if (!started) { started = true; if (walker._accept(root) === 1) return root; }
          return walker.nextNode();
        },
        previousNode() { return walker.previousNode(); },
        detach() {},
      };
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
    elementFromPoint() { return null; }
    querySelector(sel) { return wrap(N.query(this.__id, String(sel), false)); }
  }
  mixin(Document, { get children() { return nodeList(ids(elementChildren(this.__id))); } });
  for (const key of ["querySelectorAll", "getElementsByTagName", "getElementsByClassName", "append", "prepend"]) {
    Object.defineProperty(Document.prototype, key, Object.getOwnPropertyDescriptor(ParentMixin, key));
  }

  const document = wrap(N.root());
  // The template contents' owner: what it makes isn't upgraded until it's
  // copied (or moved) into the document.
  const inertDocument = Object.create(document);
  inertDocument.createElement = (tag) => {
    creatingInert = true;
    try { return wrap(N.createElement(String(tag))); } finally { creatingInert = false; }
  };
  {
    // Falsy and == undefined, like browsers' (pages detect old IE with it).
    const all = N.htmlAll();
    const elements = () => Array.from(document.getElementsByTagName("*"));
    Object.defineProperties(all, {
      length: { get: () => elements().length },
      item: { value: (i) => (typeof i === "number" ? elements()[i] : all.namedItem(i)) || null },
      namedItem: { value: (name) => elements().find((e) => e.id === name || e.getAttribute("name") === name) || null },
      [Symbol.iterator]: { value: () => elements()[Symbol.iterator]() },
    });
    Object.defineProperty(Document.prototype, "all", { configurable: true, get: () => all });
  }

  // ---- window ----------------------------------------------------------

  const window = global;
  Object.defineProperty(window, "event", { configurable: true, get: () => currentEvent, set() {} });
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

  // Web storage: the data lives in Rust (localStorage on disk per origin,
  // sessionStorage per tab); `storage.key = value` works too.
  const storage = (kind) => {
    const api = {
      getItem: (k) => N.storage(kind, "get", String(k), ""),
      setItem: (k, v) => { N.storage(kind, "set", String(k), String(v)); },
      removeItem: (k) => { N.storage(kind, "remove", String(k), ""); },
      clear: () => { N.storage(kind, "clear", "", ""); },
      key: (i) => { const keys = N.storage(kind, "keys", "", ""); return keys && keys[i] !== undefined ? keys[i] : null; },
      get length() { const keys = N.storage(kind, "keys", "", ""); return keys ? keys.length : 0; },
    };
    return new Proxy(api, {
      get(t, k) { if (k in t || typeof k !== "string") return t[k]; const v = t.getItem(k); return v === null ? undefined : v; },
      set(t, k, v) { t.setItem(k, v); return true; },
      deleteProperty(t, k) { t.removeItem(k); return true; },
      has(t, k) { return k in t || t.getItem(k) !== null; },
      ownKeys() { return N.storage(kind, "keys", "", "") || []; },
      getOwnPropertyDescriptor(t, k) { const v = t.getItem(k); return v === null ? undefined : { value: v, enumerable: true, configurable: true, writable: true }; },
    });
  };

  // A small URL implementation (enough for the common uses).
  // URL and URLSearchParams are native classes (web_url.rs).

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
      set(v) {
        const u = new URL(N.url());
        u[k] = v;
        if (k !== "hash") return N.navigate(u.href);
        // A new entry in this document, then hashchange.
        const from = N.url();
        history.pushState(null, "", u.href);
        const hc = new Event("hashchange");
        hc.oldURL = from; hc.newURL = u.href;
        window.dispatchEvent(hc);
      },
    });
  }

  // Session history for this document: pushState entries live here (with
  // their state); the tab keeps the list of URLs and asks us to traverse.
  const historyEntries = [{ url: N.url(), state: null }];
  let historyIndex = 0;
  const cloneState = (state) => {
    if (state === undefined || state === null) return null;
    try { return structuredClone(state); } catch (_) { return state; }
  };
  const history = {
    get length() { return historyEntries.length; },
    get state() { return historyEntries[historyIndex].state; },
    scrollRestoration: "auto",
    pushState(state, _title, url) {
      const href = url === undefined || url === null ? N.setUrl(N.url(), 1) : N.setUrl(String(url), 1);
      historyEntries.length = historyIndex + 1;
      historyEntries.push({ url: href, state: cloneState(state) });
      historyIndex++;
    },
    replaceState(state, _title, url) {
      const href = url === undefined || url === null ? N.url() : N.setUrl(String(url), 2);
      historyEntries[historyIndex] = { url: href, state: cloneState(state) };
    },
    back() { N.traverse(-1); },
    forward() { N.traverse(1); },
    go(n) { n = Number(n) || 0; if (n) N.traverse(n); else location.reload(); },
  };
  function traverse(delta) {
    const from = historyEntries[historyIndex].url;
    historyIndex = Math.max(0, Math.min(historyEntries.length - 1, historyIndex + delta));
    const entry = historyEntries[historyIndex];
    N.setUrl(entry.url, 0);
    const ev = new Event("popstate");
    ev.state = entry.state;
    window.dispatchEvent(ev);
    if (from.split("#")[0] === entry.url.split("#")[0] && from !== entry.url) {
      const hc = new Event("hashchange");
      hc.oldURL = from; hc.newURL = entry.url;
      window.dispatchEvent(hc);
    }
  }

  // getComputedStyle: the style engine's values (natively), with the size
  // the tab laid the element out at (or, before that, a block's estimate).
  function computedStyleOf(el) {
    const read = (name) => {
      if (!el || el.__id === undefined || el.nodeType !== 1) return "";
      name = String(name);
      const prop = name.startsWith("--") ? name : kebab(name);
      let w = -1, h = -1;
      if (prop === "width" || prop === "height") {
        const r = measure(el);
        if (r) { w = r[2]; h = r[3]; }
        else if (prop === "width") { const e = estimatedWidth(el); if (e > 0) w = e; }
      }
      return N.computedStyle(el.__id, prop, w, h, viewport[0], viewport[1]);
    };
    const api = {
      getPropertyValue: (n) => read(n), getPropertyPriority: () => "", item: () => "", length: 0, cssText: "",
      setProperty() { throw new DOMException("Computed styles are read-only", "NoModificationAllowedError"); },
      removeProperty() { throw new DOMException("Computed styles are read-only", "NoModificationAllowedError"); },
    };
    return new Proxy(api, { get(t, p) { if (p in t) return t[p]; if (typeof p !== "string") return undefined; return read(p); } });
  }

  // matchMedia: evaluated by the style engine's media queries (natively),
  // against the viewport the tab reports; lists re-check on resize and
  // fire `change` when their answer flips.
  const mediaLists = new Set();
  const mediaMatches = (q) => N.mediaMatches(q, viewport[0], viewport[1]);
  class MediaQueryList extends EventTarget {
    constructor(media) {
      super();
      this.media = media;
      this.onchange = null;
      this._last = mediaMatches(media);
    }
    get matches() { return mediaMatches(this.media); }
    addListener(cb) { if (cb) this.addEventListener("change", cb); }
    removeListener(cb) { if (cb) this.removeEventListener("change", cb); }
    addEventListener(type, cb, opts) { if (type === "change") mediaLists.add(this); super.addEventListener(type, cb, opts); }
    _recheck() {
      const now = mediaMatches(this.media);
      if (now === this._last) return;
      this._last = now;
      const ev = new Event("change");
      ev.matches = now; ev.media = this.media;
      if (typeof this.onchange === "function") { try { this.onchange(ev); } catch (e) { reportError(e); } }
      this.dispatchEvent(ev);
    }
  }
  function matchMedia(query) {
    const list = new MediaQueryList(String(query));
    if (N.debugIO && N.debugIO()) console.log("IODBG matchMedia " + list.media + " -> " + list._last);
    return list;
  }
  function recheckMedia() {
    for (const list of mediaLists) list._recheck();
  }

  // Layout the tab reports: where measured elements are ([x, y, w, h],
  // viewport-relative) and the viewport's size.
  const layoutRects = new Map();
  const measuredIds = new Set();
  let viewport = [1280, 800];
  const pageScroll = () => 0;
  const domRect = (x, y, width, height) => ({ x, y, width, height, top: y, left: x, right: x + width, bottom: y + height, toJSON() { return this; } });
  // <html> and <body> have no box of their own here: they span the viewport.
  function rootSize(el, axis) {
    if (el === document.documentElement || el === document.body) return viewport[axis];
    return axis === 0 ? estimatedWidth(el) : 0;
  }
  // The width of an element the tab hasn't laid out yet (scripts measure
  // what they just made, where a browser lays out on the spot): a block
  // fills its container, so the nearest laid-out ancestor's width, else
  // the viewport's. Inline boxes are as wide as their text: 0 here.
  // (Carousels size their slides from this: NASA's were 0px wide.)
  const INLINE_TAGS = new Set(["span", "a", "b", "i", "em", "strong", "small", "label", "img", "svg", "code", "abbr", "sup", "sub", "br", "input", "button", "select", "textarea", "video", "canvas", "iframe"]);
  const cssOf = (el, prop) => N.computedStyle(el.__id, prop, -1, -1, viewport[0], viewport[1]);
  function estimatedWidth(el) {
    if (!el || INLINE_TAGS.has(el.localName) || !N.connected(el.__id)) return 0;
    // Only a block in block flow fills its container: flex items in a row,
    // inline-level and table boxes are as wide as their content (GitHub
    // collapsed its breadcrumbs, each crumb "as wide as the page").
    if (!/^(block|flex|grid|list-item|flow-root)$/.test(cssOf(el, "display"))) return 0;
    const parent = el.parentNode;
    if (parent && parent.nodeType === 1) {
      const pd = cssOf(parent, "display");
      if (/flex|grid/.test(pd) && !(/flex/.test(pd) && /column/.test(cssOf(parent, "flex-direction")))) return 0;
      if (/inline|table/.test(pd)) return 0;
    }
    for (let n = el.parentNode; n && n.nodeType === 1; n = n.parentNode) {
      const r = painted.has(n.__id) && layoutRects.get(n.__id);
      if (r) return r[2];
    }
    return viewport[0];
  }
  function measure(el) {
    if (!el || el.__id === undefined) return null;
    if (!measuredIds.has(el.__id)) {
      measuredIds.add(el.__id);
      N.touch();
      // Never reported: ask the tab where it is now.
      if (!layoutRects.has(el.__id)) {
        const r = N.locate(el.__id);
        if (r) layoutRects.set(el.__id, r);
      }
    }
    return layoutRects.get(el.__id) || null;
  }
  const resizeObservers = new Set();
  class ResizeObserver {
    constructor(cb) { this._cb = cb; this._targets = new Map(); this._late = new Set(); }
    observe(el) {
      if (!el) return;
      this._targets.set(el, null);
      resizeObservers.add(this);
      if (measure(el)) setTimeout(deliverResizes, 0);
      // Not laid out yet: scripts saw it as 0x0 so far.
      else this._late.add(el);
    }
    unobserve(el) { this._targets.delete(el); }
    disconnect() { this._targets.clear(); resizeObservers.delete(this); }
    takeRecords() { return []; }
  }
  // ResizeObserver callbacks for targets whose size is new.
  function deliverResizes() {
    for (const ro of resizeObservers) {
      const entries = [];
      for (const [el, last] of ro._targets) {
        const r = layoutRects.get(el.__id);
        if (!r || (last && last[0] === r[2] && last[1] === r[3])) continue;
        ro._targets.set(el, [r[2], r[3]]);
        const size = [{ inlineSize: r[2], blockSize: r[3] }];
        entries.push({ target: el, contentRect: domRect(0, 0, r[2], r[3]), borderBoxSize: size, contentBoxSize: size, devicePixelContentBoxSize: size });
      }
      if (entries.length) { try { ro._cb(entries, ro); } catch (e) { reportError(e); } }
      // A size that arrived after observe() also counts as a change from the
      // 0x0 scripts saw (the first callback is often taken as the initial one).
      let again = false;
      for (const e of entries) {
        if (ro._late.delete(e.target)) { ro._targets.set(e.target, null); again = true; }
      }
      if (again) setTimeout(deliverResizes, 16);
    }
  }
  // Elements painted in the tab's latest frame (layoutRects keeps the last
  // rect of ones that no longer are).
  const painted = new Set();
  // Measured elements the tab says it didn't paint (off screen, or no box).
  const unpainted = new Set();
  function layout(flat, vw, vh, gone) {
    const resized = viewport[0] !== vw || viewport[1] !== vh;
    viewport = [vw, vh];
    for (let i = 0; i + 4 < flat.length; i += 5) {
      layoutRects.set(flat[i], [flat[i + 1], flat[i + 2], flat[i + 3], flat[i + 4]]);
      painted.add(flat[i]);
      unpainted.delete(flat[i]);
    }
    if (gone) for (const id of gone) { painted.delete(id); unpainted.add(id); }
    deliverResizes();
    deliverIntersections();
    if (resized) { recheckMedia(); window.dispatchEvent(new Event("resize")); }
  }

  // DOM traversal (document.createTreeWalker / createNodeIterator).
  const NodeFilter = {
    FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3,
    SHOW_ALL: 0xffffffff, SHOW_ELEMENT: 0x1, SHOW_ATTRIBUTE: 0x2, SHOW_TEXT: 0x4, SHOW_CDATA_SECTION: 0x8,
    SHOW_PROCESSING_INSTRUCTION: 0x40, SHOW_COMMENT: 0x80, SHOW_DOCUMENT: 0x100, SHOW_DOCUMENT_TYPE: 0x200, SHOW_DOCUMENT_FRAGMENT: 0x400,
  };
  class TreeWalker {
    constructor(root, whatToShow, filter) {
      this.root = root;
      this.whatToShow = whatToShow === undefined ? NodeFilter.SHOW_ALL : whatToShow >>> 0;
      this.filter = filter || null;
      this.currentNode = root;
    }
    _accept(node) {
      if (!(this.whatToShow & (1 << (node.nodeType - 1)))) return NodeFilter.FILTER_SKIP;
      if (!this.filter) return NodeFilter.FILTER_ACCEPT;
      const f = typeof this.filter === "function" ? this.filter : this.filter.acceptNode.bind(this.filter);
      return f(node) || NodeFilter.FILTER_ACCEPT;
    }
    // Document order, staying under the root.
    _next(node, skipChildren) {
      if (!skipChildren && node.firstChild) return node.firstChild;
      for (let n = node; n && n !== this.root; n = n.parentNode) if (n.nextSibling) return n.nextSibling;
      return null;
    }
    nextNode() {
      let n = this.currentNode, skip = false;
      for (;;) {
        n = this._next(n, skip);
        if (!n) return null;
        const r = this._accept(n);
        if (r === NodeFilter.FILTER_ACCEPT) return (this.currentNode = n);
        skip = r === NodeFilter.FILTER_REJECT;
      }
    }
    previousNode() {
      let n = this.currentNode;
      while (n && n !== this.root) {
        let prev = n.previousSibling;
        if (prev) { while (prev.lastChild) prev = prev.lastChild; n = prev; } else n = n.parentNode;
        if (n && this._accept(n) === NodeFilter.FILTER_ACCEPT) return (this.currentNode = n);
      }
      return null;
    }
    parentNode() {
      for (let n = this.currentNode; n && n !== this.root; ) { n = n.parentNode; if (n && this._accept(n) === NodeFilter.FILTER_ACCEPT) return (this.currentNode = n); }
      return null;
    }
    firstChild() { for (let c = this.currentNode.firstChild; c; c = c.nextSibling) if (this._accept(c) === NodeFilter.FILTER_ACCEPT) return (this.currentNode = c); return null; }
    lastChild() { for (let c = this.currentNode.lastChild; c; c = c.previousSibling) if (this._accept(c) === NodeFilter.FILTER_ACCEPT) return (this.currentNode = c); return null; }
    nextSibling() { for (let c = this.currentNode.nextSibling; c; c = c.nextSibling) if (this._accept(c) === NodeFilter.FILTER_ACCEPT) return (this.currentNode = c); return null; }
    previousSibling() { for (let c = this.currentNode.previousSibling; c; c = c.previousSibling) if (this._accept(c) === NodeFilter.FILTER_ACCEPT) return (this.currentNode = c); return null; }
  }

  // Real browsers' entry class (polyfills replace observers without it).
  class IntersectionObserverEntry {
    constructor(init) { Object.assign(this, init); }
  }
  for (const k of ["target", "isIntersecting", "intersectionRatio", "boundingClientRect", "intersectionRect", "rootBounds", "time"]) {
    Object.defineProperty(IntersectionObserverEntry.prototype, k, { configurable: true, writable: true, value: undefined });
  }
  const observer = (name) => class {
    constructor(cb) { this._cb = cb; }
    observe() {}
    unobserve() {} disconnect() {} takeRecords() { return []; }
  };

  // IntersectionObserver from the tab's layout: a target intersects when the
  // box it was last painted in overlaps the viewport (grown by rootMargin).
  // One never painted is off-screen: the page list only draws what's in view
  // (and measured elements always get a box). Without this, everything was
  // "visible", and infinite feeds loaded page after page in the background.
  const intersectionObservers = new Set();
  const NEAR_ANCESTORS = 4;
  // Where `el` is painted now: its own box if it has ever had one (not
  // painted now: nowhere); a box that never got one (a 0x0 lazy <img>)
  // goes by its nearest painted ancestor.
  function paintedRect(el) {
    if (unpainted.has(el.__id)) return undefined;
    if (layoutRects.has(el.__id)) return painted.has(el.__id) ? layoutRects.get(el.__id) : undefined;
    for (let n = el.parentNode, i = 1; n && n.nodeType === 1 && i <= NEAR_ANCESTORS; n = n.parentNode, i++) {
      const r = painted.has(n.__id) && layoutRects.get(n.__id);
      if (r) return r;
    }
    return undefined;
  }
  class IntersectionObserver {
    constructor(cb, opts = {}) {
      this._cb = cb;
      this._targets = new Map(); // element -> last isIntersecting (undefined: not reported yet)
      this.root = opts.root || null;
      this.rootMargin = opts.rootMargin || "0px";
      const t = opts.threshold;
      this.thresholds = Array.isArray(t) ? t.slice() : [typeof t === "number" ? t : 0];
      const parts = String(this.rootMargin).trim().split(/\s+/).map((v) => ({ n: parseFloat(v) || 0, pct: /%$/.test(v) }));
      const [top, right = top, bottom = top, left = right] = parts;
      this._margin = { top, right, bottom, left };
    }
    observe(target) {
      if (!target || target.__id === undefined || this._targets.has(target)) return;
      this._targets.set(target, undefined);
      intersectionObservers.add(this);
      // Its nearest ancestors too: a box with nothing in it yet (an <img>
      // waiting for its `src`) may not be painted, but where they are, it is.
      for (let n = target, i = 0; n && n.nodeType === 1 && i <= NEAR_ANCESTORS; n = n.parentNode, i++) measure(n);
      setTimeout(() => this._check(false), 0);
      // The tab says where it's painted, or that it isn't (off screen,
      // nothing rendered). Should it never say (no layout comes), report
      // anyway: callers wait for a first entry.
      setTimeout(() => this._check(target), 3000);
    }
    unobserve(target) { this._targets.delete(target); }
    disconnect() { this._targets.clear(); intersectionObservers.delete(this); }
    takeRecords() { return []; }
    // `settle`: the target whose wait for a first paint is over (it's
    // reported even if it never got one).
    _check(settle) {
      const [vw, vh] = viewport;
      const rootRect = this.root && this.root.__id !== undefined ? layoutRects.get(this.root.__id) : null;
      const [rx, ry, rw, rh] = rootRect || [0, 0, vw, vh];
      const px = (m, size) => (m.pct ? (m.n * size) / 100 : m.n);
      const m = this._margin;
      const box = { left: rx - px(m.left, rw), top: ry - px(m.top, rh), right: rx + rw + px(m.right, rw), bottom: ry + rh + px(m.bottom, rh) };
      const entries = [];
      for (const [target, last] of this._targets) {
        const r = paintedRect(target);
        if (settle && settle !== target) continue;
        if (!r && !settle && !unpainted.has(target.__id)) continue;
        const [x, y, w, h] = r || [0, 0, 0, 0];
        const ix = Math.max(x, box.left), iy = Math.max(y, box.top);
        const ax = Math.min(x + w, box.right), ay = Math.min(y + h, box.bottom);
        const inside = !!r && N.connected(target.__id) && ax >= ix && ay >= iy;
        const area = w * h;
        const ratio = !inside ? 0 : area > 0 ? ((ax - ix) * (ay - iy)) / area : 1;
        const visible = inside && this.thresholds.some((t) => (t === 0 ? true : ratio >= t));
        if (last === visible) continue;
        if (N.debugIO && N.debugIO() && target.localName === "img") console.log("IODBG img " + (visible ? "in" : "out") + " t=" + Math.round(N.now()) + " r=" + JSON.stringify(r) + " root=" + (this.root ? this.root.localName : "vp") + " th=" + JSON.stringify(this.thresholds) + " margin=" + this.rootMargin + " src=" + (target.getAttribute("src") ? "y" : "n"));
        if (visible && N.debugIO && N.debugIO()) console.log("IODBG visible " + target.localName + "." + String(target.className).slice(0, 60) + " r=" + JSON.stringify(r) + " own=" + JSON.stringify(layoutRects.get(target.__id) || null) + " vp=" + viewport + " box=" + JSON.stringify(box));
        this._targets.set(target, visible);
        entries.push(new IntersectionObserverEntry({
          target, isIntersecting: visible, intersectionRatio: ratio, time: N.now(),
          boundingClientRect: new DOMRect(x, y, w, h),
          intersectionRect: inside ? new DOMRect(ix, iy, ax - ix, ay - iy) : new DOMRect(0, 0, 0, 0),
          rootBounds: new DOMRect(rx, ry, rw, rh),
        }));
      }
      if (entries.length) { try { this._cb(entries, this); } catch (e) { reportError(e); } }
    }
  }
  function deliverIntersections() {
    for (const io of intersectionObservers) io._check(false);
  }

  // Text selection: there's none we know of (the tab owns it).
  const selection = {
    rangeCount: 0, isCollapsed: true, type: "None", anchorNode: null, focusNode: null, anchorOffset: 0, focusOffset: 0,
    removeAllRanges() {}, addRange() {}, collapse() {}, collapseToEnd() {}, collapseToStart() {}, extend() {},
    selectAllChildren() {}, deleteFromDocument() {}, containsNode() { return false; },
    getRangeAt() { throw new DOMException("No range", "IndexSizeError"); },
    toString() { return ""; },
  };
  const getSelection = () => selection;

  const consoleImpl = {};
  const fmt = (args) => args.map((a) => {
    if (typeof a === "string") return a;
    if (a instanceof Error) return String(a) + (a.stack ? "\n" + a.stack : "");
    if (isNode(a)) return "<" + (a.localName || a.nodeName) + ">";
    try { return JSON.stringify(a); } catch (_) { return String(a); }
  }).join(" ");
  for (const level of ["log", "info", "warn", "debug"]) consoleImpl[level] = (...a) => N.log(level, fmt(a));
  // Where an error was reported from, too: pages often log bare stacks.
  for (const level of ["error", "trace"]) consoleImpl[level] = (...a) => {
    let where = "";
    try { where = "\n  (logged " + String(new Error().stack).split("\n").slice(1, 4).join(" <- ").replace(/\s+/g, " ") + ")"; } catch (_) {}
    N.log(level, "[" + a.map((x) => typeof x).join(",") + "] " + fmt(a) + where);
  };
  for (const name of ["group", "groupCollapsed", "groupEnd", "time", "timeEnd", "count", "assert", "table", "dir"]) consoleImpl[name] = () => {};

  const navigatorImpl = {
    userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Feather/0.1 Safari/537.36",
    language: "en-US", languages: ["en-US", "en"], platform: "MacIntel", vendor: "", onLine: true,
    cookieEnabled: true, hardwareConcurrency: 8, maxTouchPoints: 0, webdriver: false,
    sendBeacon(url, data) { fetch(url, { method: "POST", body: data, mode: "no-cors", credentials: "include" }).catch(() => {}); return true; },
    clipboard: { writeText: () => Promise.resolve(), readText: () => Promise.resolve("") },
    serviceWorker: undefined,
  };

  // Where the page is scrolled, as the tab last said (getters: assigning
  // them would copy one value).
  for (const [name, get] of [["scrollX", N.scrollX], ["pageXOffset", N.scrollX], ["scrollY", N.scrollY], ["pageYOffset", N.scrollY]]) {
    Object.defineProperty(window, name, { get, configurable: true });
  }
  Object.assign(window, {
    window, self: window, globalThis: window, top: window, parent: window, frames: window,
    document, location, history, navigator: navigatorImpl, console: consoleImpl, getSelection,
    Window, HTMLDocument: Document,
    Node, Element, HTMLElement, Text, Comment, Document, DocumentFragment, CharacterData, EventTarget,
    CDATASection, ProcessingInstruction, DocumentType, Attr,
    HTMLAnchorElement, HTMLImageElement, Image, HTMLScriptElement, HTMLLinkElement, HTMLStyleElement, HTMLInputElement,
    HTMLTextAreaElement, HTMLSelectElement, HTMLOptionElement, HTMLButtonElement, HTMLFormElement,
    HTMLTemplateElement, HTMLCanvasElement, HTMLMediaElement, HTMLVideoElement, HTMLAudioElement, TextTrack, TimeRanges,
    HTMLIFrameElement, HTMLDivElement, HTMLSpanElement, SVGElement, SVGSVGElement, SVGPathElement, SVGGElement,
    HTMLParagraphElement, HTMLHeadingElement, HTMLUListElement, HTMLOListElement, HTMLLIElement, HTMLTableElement,
    HTMLTableRowElement, HTMLTableCellElement, HTMLLabelElement, HTMLPictureElement, HTMLSourceElement, HTMLSlotElement,
    HTMLUnknownElement, HTMLBodyElement, HTMLHeadElement, HTMLHtmlElement, HTMLMetaElement, HTMLBRElement, HTMLHRElement,
    HTMLPreElement, HTMLDialogElement, HTMLDetailsElement,
    ...Object.fromEntries(Object.entries(moreElementClasses).filter(([k]) => /^HTML/.test(k))),
    NodeList, HTMLCollection: NodeList, DOMTokenList,
    // Interfaces checked with `instanceof` / `typeof` (nothing makes most of these yet).
    FileList: class FileList extends Array { item(i) { return this[i] || null; } },
    DataTransfer: class DataTransfer {
      constructor() { this._data = new Map(); this.items = []; this.files = []; this.types = []; this.dropEffect = "none"; this.effectAllowed = "all"; }
      setData(type, value) { this._data.set(type, String(value)); this.types = Array.from(this._data.keys()); }
      getData(type) { return this._data.get(type) || ""; }
      clearData(type) { if (type) this._data.delete(type); else this._data.clear(); this.types = Array.from(this._data.keys()); }
      setDragImage() {}
    },
    ImageData: class ImageData {
      constructor(a, b, c) {
        if (a instanceof Uint8ClampedArray) { this.data = a; this.width = b; this.height = c ?? a.length / 4 / b; }
        else { this.width = a; this.height = b; this.data = new Uint8ClampedArray(a * b * 4); }
        this.colorSpace = "srgb";
      }
    },
    CustomElementRegistry: class CustomElementRegistry {},
    ResizeObserverEntry: class ResizeObserverEntry {},
    CompositionEvent: class CompositionEvent extends UIEvent { constructor(t, i = {}) { super(t, i); this.data = i.data || ""; } },
    SubmitEvent: class SubmitEvent extends Event { constructor(t, i = {}) { super(t, i); this.submitter = i.submitter || null; } },
    XPathResult: Object.assign(class XPathResult {}, { ANY_TYPE: 0, NUMBER_TYPE: 1, STRING_TYPE: 2, BOOLEAN_TYPE: 3, UNORDERED_NODE_ITERATOR_TYPE: 4, ORDERED_NODE_ITERATOR_TYPE: 5, UNORDERED_NODE_SNAPSHOT_TYPE: 6, ORDERED_NODE_SNAPSHOT_TYPE: 7, ANY_UNORDERED_NODE_TYPE: 8, FIRST_ORDERED_NODE_TYPE: 9 }),
    VisualViewport: class VisualViewport extends EventTarget {},
    ReportingObserver: class ReportingObserver { observe() {} disconnect() {} takeRecords() { return []; } },
    CSSRule,
    XMLDocument: Document, HTMLDocument: Document,
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
    performance: (() => {
      const origin = Date.now() - N.now();
      const t = Math.round(origin);
      // Navigation Timing (old and new style): one plausible page load.
      const timing = {
        navigationStart: t, fetchStart: t, domainLookupStart: t, domainLookupEnd: t, connectStart: t, connectEnd: t, secureConnectionStart: t,
        requestStart: t, responseStart: t, responseEnd: t, domLoading: t, domInteractive: t, domContentLoadedEventStart: t,
        domContentLoadedEventEnd: t, domComplete: t, loadEventStart: t, loadEventEnd: t, unloadEventStart: 0, unloadEventEnd: 0,
        redirectStart: 0, redirectEnd: 0, toJSON() { return this; },
      };
      const navigation = { name: location.href, entryType: "navigation", startTime: 0, duration: 0, type: "navigate", redirectCount: 0, transferSize: 0, encodedBodySize: 0, decodedBodySize: 0, responseStart: 0, domContentLoadedEventEnd: 0, loadEventEnd: 0, toJSON() { return this; } };
      const marks = [];
      return {
        now: () => N.now(), timeOrigin: origin, timing, navigation: { type: 0, redirectCount: 0 },
        mark(name, opts) { const m = { name, entryType: "mark", startTime: N.now(), duration: 0, detail: opts && opts.detail }; marks.push(m); return m; },
        measure(name) { const m = { name, entryType: "measure", startTime: 0, duration: N.now() }; marks.push(m); return m; },
        clearMarks() { marks.length = 0; }, clearMeasures() {},
        getEntries() { return [navigation, ...marks]; },
        getEntriesByType(type) { return type === "navigation" ? [navigation] : marks.filter((m) => m.entryType === type); },
        getEntriesByName(name) { return marks.filter((m) => m.name === name); },
        setResourceTimingBufferSize() {}, toJSON() { return { timeOrigin: origin, timing }; },
        addEventListener() {}, removeEventListener() {},
      };
    })(),
    localStorage: storage("local"), sessionStorage: storage("session"),
    matchMedia,
    getComputedStyle: computedStyleOf,
    MutationObserver, MutationRecord: class MutationRecord {},
    IntersectionObserver, IntersectionObserverEntry,
    ResizeObserver, NodeFilter, TreeWalker,
    PerformanceObserver: observer("PerformanceObserver"),
    outerWidth: 1280, outerHeight: 900, devicePixelRatio: 2,
    screen: { width: 1512, height: 982, availWidth: 1512, availHeight: 950, colorDepth: 24 },
    scrollTo: N.scrollTo, scrollBy: N.scrollBy, scroll: N.scrollTo,
    alert() {}, confirm() { return false; }, prompt() { return null; },
    open() { return null; }, close() {}, focus() {}, blur() {}, print() {},
    // Messages to this window arrive asynchronously, as `message` events.
    postMessage(data, origin) {
      setTimeout(() => {
        const ev = new MessageEvent("message", { data, origin: location.origin, source: window });
        window.dispatchEvent(ev);
      }, 0);
    },
    atob: (s) => { const r = N.atob(String(s)); if (r === null) throw new DOMException("The string to be decoded is not correctly encoded.", "InvalidCharacterError"); return r; },
    btoa: (s) => { const r = N.btoa(String(s)); if (r === null) throw new DOMException("The string to be encoded contains characters outside of the Latin1 range.", "InvalidCharacterError"); return r; },
    reportError,
    isSecureContext: true,
    customElements, ShadowRoot, CSSStyleSheet,
  });

  // ---- crypto ---------------------------------------------------------
  // Real randomness and real algorithms (ring), bytes as latin1 strings.
  const bytesOf = (data) => {
    if (data instanceof ArrayBuffer) return new Uint8Array(data);
    if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    throw new TypeError("expected an ArrayBuffer or a view of one");
  };
  const toLatin1 = (data) => latin1Of(bytesOf(data));
  const latin1Of = (bytes) => { let s = ""; for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]); return s; };
  const bufferOf = (text) => { const b = new Uint8Array(text.length); for (let i = 0; i < text.length; i++) b[i] = text.charCodeAt(i); return b.buffer; };
  const algName = (a) => (typeof a === "string" ? a : a && a.name || "").toUpperCase();
  const hashName = (h) => algName(h).replace("-", "") || "SHA256";
  class CryptoKey {
    constructor(type, algorithm, extractable, usages, data) { this.type = type; this.algorithm = algorithm; this.extractable = extractable; this.usages = usages; Object.defineProperty(this, "_data", { value: data }); }
  }
  const notSupported = (what) => Promise.reject(new DOMException(what + " isn't supported", "NotSupportedError"));
  const subtle = {
    digest(alg, data) { try { return Promise.resolve(bufferOf(N.digest(algName(alg), toLatin1(data)))); } catch (e) { return Promise.reject(e); } },
    generateKey(alg, extractable, usages) {
      const name = algName(alg);
      if (name === "ED25519") {
        const [pkcs8, pub] = N.ed25519Generate();
        const algorithm = { name: "Ed25519" };
        return Promise.resolve({
          privateKey: new CryptoKey("private", algorithm, extractable, usages.filter((u) => u === "sign"), pkcs8),
          publicKey: new CryptoKey("public", algorithm, true, usages.filter((u) => u === "verify"), pub),
        });
      }
      if (name === "HMAC") {
        const length = alg.length || ({ SHA1: 512, SHA256: 512, SHA384: 1024, SHA512: 1024 }[hashName(alg.hash)] || 512);
        return Promise.resolve(new CryptoKey("secret", { name: "HMAC", hash: { name: algName(alg.hash) }, length }, extractable, usages, N.randomBytes(length / 8)));
      }
      return notSupported(name);
    },
    importKey(format, keyData, alg, extractable, usages) {
      const name = algName(alg);
      try {
        if (format === "jwk") return notSupported("jwk keys");
        const raw = toLatin1(keyData);
        if (name === "HMAC") return Promise.resolve(new CryptoKey("secret", { name: "HMAC", hash: { name: algName(alg.hash) }, length: raw.length * 8 }, extractable, usages, raw));
        if (name === "ED25519") {
          if (format === "raw" || format === "spki") return Promise.resolve(new CryptoKey("public", { name: "Ed25519" }, true, usages, format === "spki" ? raw.slice(-32) : raw));
          if (format === "pkcs8") { N.ed25519Public(raw); return Promise.resolve(new CryptoKey("private", { name: "Ed25519" }, extractable, usages, raw)); }
        }
      } catch (e) { return Promise.reject(e); }
      return notSupported(name + " " + format + " keys");
    },
    exportKey(format, key) {
      if (!key.extractable) return Promise.reject(new DOMException("key is not extractable", "InvalidAccessError"));
      if (key.algorithm.name === "Ed25519" && key.type === "private" && format === "raw") return Promise.resolve(bufferOf(N.ed25519Public(key._data)));
      if (format === "raw" || format === "pkcs8") return Promise.resolve(bufferOf(key._data));
      return notSupported(format + " export");
    },
    sign(alg, key, data) {
      try {
        const name = algName(alg);
        if (name === "ED25519") return Promise.resolve(bufferOf(N.ed25519Sign(key._data, toLatin1(data))));
        if (name === "HMAC") return Promise.resolve(bufferOf(N.hmac(hashName(key.algorithm.hash), key._data, toLatin1(data))));
        return notSupported(name);
      } catch (e) { return Promise.reject(e); }
    },
    verify(alg, key, signature, data) {
      try {
        const name = algName(alg);
        if (name === "ED25519") return Promise.resolve(N.ed25519Verify(key._data, toLatin1(data), toLatin1(signature)));
        if (name === "HMAC") return Promise.resolve(N.hmac(hashName(key.algorithm.hash), key._data, toLatin1(data)) === toLatin1(signature));
        return notSupported(name);
      } catch (e) { return Promise.reject(e); }
    },
    encrypt() { return notSupported("encrypt"); },
    decrypt() { return notSupported("decrypt"); },
    deriveKey() { return notSupported("deriveKey"); },
    deriveBits() { return notSupported("deriveBits"); },
    wrapKey() { return notSupported("wrapKey"); },
    unwrapKey() { return notSupported("unwrapKey"); },
  };
  const cryptoImpl = {
    subtle,
    getRandomValues(arr) {
      const bytes = N.randomBytes(arr.byteLength);
      const view = new Uint8Array(arr.buffer, arr.byteOffset, arr.byteLength);
      for (let i = 0; i < view.length; i++) view[i] = bytes.charCodeAt(i);
      return arr;
    },
    randomUUID() {
      const b = N.randomBytes(16).split("").map((c) => c.charCodeAt(0));
      b[6] = (b[6] & 15) | 64; b[8] = (b[8] & 63) | 128;
      const h = b.map((x) => x.toString(16).padStart(2, "0")).join("");
      return h.slice(0, 8) + "-" + h.slice(8, 12) + "-" + h.slice(12, 16) + "-" + h.slice(16, 20) + "-" + h.slice(20);
    },
  };

  Object.assign(window, { crypto: cryptoImpl, CryptoKey });
  // The viewport's size, as the tab last reported it.
  for (const [name, i] of [["innerWidth", 0], ["innerHeight", 1]]) {
    Object.defineProperty(window, name, { configurable: true, get: () => viewport[i], set() {} });
  }

  // ---- network -----------------------------------------------------------

  class Headers {
    constructor(init) {
      this._map = new Map();
      if (init instanceof Headers) init.forEach((v, k) => this.append(k, v));
      else if (Array.isArray(init)) for (const [k, v] of init) this.append(k, v);
      else if (init && typeof init === "object") for (const k of Object.keys(init)) this.append(k, init[k]);
    }
    append(k, v) { k = String(k).toLowerCase(); const old = this._map.get(k); this._map.set(k, old === undefined ? String(v) : old + ", " + v); }
    set(k, v) { this._map.set(String(k).toLowerCase(), String(v)); }
    get(k) { const v = this._map.get(String(k).toLowerCase()); return v === undefined ? null : v; }
    has(k) { return this._map.has(String(k).toLowerCase()); }
    delete(k) { this._map.delete(String(k).toLowerCase()); }
    forEach(fn, self) { for (const [k, v] of this._map) fn.call(self, v, k, this); }
    keys() { return this._map.keys(); }
    values() { return this._map.values(); }
    entries() { return this._map.entries(); }
    [Symbol.iterator]() { return this._map.entries(); }
    _list() { return Array.from(this._map.entries()); }
  }

  // UTF-8 both ways (Boa has no TextEncoder).
  // TextEncoder and TextDecoder are native classes (encoding.rs).

  // Streams: enough for reading bodies chunk by chunk (NDJSON, SSE-ish
  // protocols over fetch) and piping through text decoding.
  class ReadableStream {
    constructor(source) {
      this._queue = []; this._waiters = []; this._done = false; this._error = null; this.locked = false;
      const controller = {
        enqueue: (c) => this._push(c), close: () => this._close(), error: (e) => this._fail(e),
        get desiredSize() { return 1; },
      };
      this._controller = controller;
      this._pull = source && source.pull ? () => { try { source.pull(controller); } catch (e) { this._fail(e); } } : null;
      this._cancel = source && source.cancel ? (r) => source.cancel(r) : null;
      if (source && source.start) { try { source.start(controller); } catch (e) { this._fail(e); } }
    }
    _push(c) { if (this._done) return; const w = this._waiters.shift(); if (w) w.resolve({ value: c, done: false }); else this._queue.push(c); }
    _close() { if (this._done) return; this._done = true; for (const w of this._waiters.splice(0)) w.resolve({ value: undefined, done: true }); }
    _fail(e) { this._error = e; for (const w of this._waiters.splice(0)) w.reject(e); }
    getReader() {
      if (this.locked) throw new TypeError("ReadableStream is locked");
      this.locked = true;
      const s = this;
      return {
        read() {
          if (s._queue.length) return Promise.resolve({ value: s._queue.shift(), done: false });
          if (s._error) return Promise.reject(s._error);
          if (s._done) return Promise.resolve({ value: undefined, done: true });
          return new Promise((resolve, reject) => { s._waiters.push({ resolve, reject }); if (s._pull) s._pull(); });
        },
        releaseLock() { s.locked = false; },
        cancel(r) { if (s._cancel) s._cancel(r); s._queue = []; s._close(); return Promise.resolve(); },
        get closed() { return new Promise((r) => { const t = setInterval(() => { if (s._done) { clearInterval(t); r(); } }, 10); }); },
      };
    }
    cancel(r) { if (this._cancel) this._cancel(r); this._close(); return Promise.resolve(); }
    pipeThrough(transform) {
      this.pipeTo(transform.writable).catch((e) => transform.readable._fail(e));
      return transform.readable;
    }
    async pipeTo(writable) {
      const reader = this.getReader();
      const writer = writable.getWriter();
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        await writer.write(value);
      }
      await writer.close();
    }
    tee() {
      const a = new ReadableStream(), b = new ReadableStream();
      (async () => { const r = this.getReader(); for (;;) { const { value, done } = await r.read(); if (done) { a._close(); b._close(); return; } a._push(value); b._push(value); } })();
      return [a, b];
    }
    async *[Symbol.asyncIterator]() {
      const reader = this.getReader();
      try { for (;;) { const { value, done } = await reader.read(); if (done) return; yield value; } } finally { reader.releaseLock(); }
    }
    static from(iterable) {
      return new ReadableStream({ async start(c) { for await (const x of iterable) c.enqueue(x); c.close(); } });
    }
  }
  class WritableStream {
    constructor(sink) { this._sink = sink || {}; this.locked = false; }
    getWriter() {
      const sink = this._sink;
      return {
        write: (c) => Promise.resolve(sink.write && sink.write(c)),
        close: () => Promise.resolve(sink.close && sink.close()),
        abort: (r) => Promise.resolve(sink.abort && sink.abort(r)),
        releaseLock() {}, get ready() { return Promise.resolve(); }, get desiredSize() { return 1; },
      };
    }
  }
  class TransformStream {
    constructor(transformer) {
      transformer = transformer || {};
      const readable = new ReadableStream();
      const controller = { enqueue: (c) => readable._push(c), error: (e) => readable._fail(e), terminate: () => readable._close() };
      if (transformer.start) transformer.start(controller);
      this.readable = readable;
      this.writable = new WritableStream({
        write: (c) => (transformer.transform ? transformer.transform(c, controller) : controller.enqueue(c)),
        close: () => { if (transformer.flush) transformer.flush(controller); readable._close(); },
        abort: (e) => readable._fail(e),
      });
    }
  }
  class TextDecoderStream extends TransformStream {
    constructor(label, opts) {
      const decoder = new TextDecoder(label, opts);
      super({
        transform(chunk, c) { const t = decoder.decode(chunk, { stream: true }); if (t) c.enqueue(t); },
        flush(c) { const t = decoder.decode(); if (t) c.enqueue(t); },
      });
      this.encoding = decoder.encoding;
    }
  }
  class TextEncoderStream extends TransformStream {
    constructor() { const e = new TextEncoder(); super({ transform(chunk, c) { c.enqueue(e.encode(chunk)); } }); this.encoding = "utf-8"; }
  }

  class Blob {
    constructor(parts, opts) {
      this._text = (parts || []).map((p) => (p instanceof Blob ? p._text : typeof p === "string" ? p : new TextDecoder().decode(p))).join("");
      this.type = (opts && opts.type) || "";
    }
    get size() { return new TextEncoder().encode(this._text).length; }
    text() { return Promise.resolve(this._text); }
    arrayBuffer() { return Promise.resolve(new TextEncoder().encode(this._text).buffer); }
    slice(a, b) { return new Blob([this._text.slice(a, b)], { type: this.type }); }
  }
  class File extends Blob {
    constructor(parts, name, opts) { super(parts, opts); this.name = name; this.lastModified = Date.now(); }
  }

  class FormData {
    constructor(form) {
      this._list = [];
      if (form && form.__id !== undefined) {
        for (const el of form.querySelectorAll("input, select, textarea")) {
          const name = el.getAttribute("name");
          if (!name || el.hasAttribute("disabled")) continue;
          const type = (el.getAttribute("type") || "").toLowerCase();
          if ((type === "checkbox" || type === "radio") && !el.checked) continue;
          if (type === "submit" || type === "button" || type === "reset" || type === "file" || type === "image") continue;
          this._list.push([name, type === "checkbox" && !el.hasAttribute("value") ? "on" : String(el.value)]);
        }
      }
    }
    append(k, v) { this._list.push([String(k), v instanceof Blob ? v : String(v)]); }
    set(k, v) { this.delete(k); this.append(k, v); }
    get(k) { const e = this._list.find(([a]) => a === k); return e ? e[1] : null; }
    getAll(k) { return this._list.filter(([a]) => a === k).map(([, v]) => v); }
    has(k) { return this._list.some(([a]) => a === k); }
    delete(k) { this._list = this._list.filter(([a]) => a !== k); }
    forEach(fn, self) { for (const [k, v] of this._list) fn.call(self, v, k, this); }
    keys() { return this._list.map(([k]) => k)[Symbol.iterator](); }
    values() { return this._list.map(([, v]) => v)[Symbol.iterator](); }
    entries() { return this._list.map((e) => e.slice())[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
  }

  // A body (string, URLSearchParams, FormData, Blob, typed array) as
  // text, plus the content type it implies.
  function encodeBody(body) {
    if (body === undefined || body === null) return [null, null];
    if (typeof body === "string") return [body, "text/plain;charset=UTF-8"];
    if (body instanceof URLSearchParams) return [body.toString(), "application/x-www-form-urlencoded;charset=UTF-8"];
    if (body instanceof FormData) {
      const boundary = "----FeatherFormBoundary" + Math.random().toString(16).slice(2);
      let out = "";
      for (const [k, v] of body._list) {
        out += "--" + boundary + "\r\n";
        if (v instanceof Blob) out += 'Content-Disposition: form-data; name="' + k + '"; filename="' + (v.name || "blob") + '"\r\nContent-Type: ' + (v.type || "application/octet-stream") + "\r\n\r\n" + v._text + "\r\n";
        else out += 'Content-Disposition: form-data; name="' + k + '"\r\n\r\n' + v + "\r\n";
      }
      return [out + "--" + boundary + "--\r\n", "multipart/form-data; boundary=" + boundary];
    }
    if (body instanceof Blob) return [body._text, body.type || null];
    if (body instanceof ArrayBuffer || ArrayBuffer.isView(body)) return [new TextDecoder().decode(body instanceof ArrayBuffer ? new Uint8Array(body) : body), null];
    return [String(body), "text/plain;charset=UTF-8"];
  }

  // A request body for the network: bytes stay bytes (gzipped JSON,
  // protobuf), passed as a byte string the native side unpacks.
  function requestBody(body) {
    if (body instanceof ArrayBuffer || ArrayBuffer.isView(body)) {
      const b = body instanceof ArrayBuffer ? new Uint8Array(body) : new Uint8Array(body.buffer, body.byteOffset, body.byteLength);
      let s = "";
      for (let i = 0; i < b.length; i += 8192) s += String.fromCharCode.apply(null, b.subarray(i, i + 8192));
      return [{ bytes: s }, null];
    }
    return encodeBody(body);
  }

  class Request {
    constructor(input, init) {
      init = init || {};
      const from = input instanceof Request ? input : null;
      this.url = from ? from.url : (N.resolve(String(input)) || String(input));
      this.method = String(init.method || (from && from.method) || "GET").toUpperCase();
      this.headers = new Headers(init.headers || (from && from.headers));
      this._body = init.body !== undefined ? init.body : from ? from._body : null;
      this.mode = init.mode || (from && from.mode) || "cors";
      this.credentials = init.credentials || (from && from.credentials) || "same-origin";
      this.cache = init.cache || "default";
      this.redirect = init.redirect || "follow";
      this.signal = init.signal || (from && from.signal) || null;
      this.referrer = "about:client";
    }
    clone() { return new Request(this); }
  }

  class Response {
    constructor(body, init) {
      init = init || {};
      this._text = body === undefined || body === null ? "" : encodeBody(body)[0];
      this.status = init.status === undefined ? 200 : init.status;
      this.statusText = init.statusText || "";
      this.headers = init.headers instanceof Headers ? init.headers : new Headers(init.headers);
      this.type = "default";
      this.url = "";
      this.redirected = false;
      this.bodyUsed = false;
    }
    get ok() { return this.status >= 200 && this.status < 300; }
    // A fetched response's body arrives later: `_full` settles with it.
    _consume() {
      if (this.bodyUsed) return Promise.reject(new TypeError("Body has already been consumed."));
      this.bodyUsed = true;
      return this._full || Promise.resolve(this._text);
    }
    get body() {
      if (this._stream) return this._stream;
      const stream = (this._stream = new ReadableStream());
      if (this._id !== undefined && !this._finished) N.fetchStream(this._id);
      else { if (this._text) stream._push(new TextEncoder().encode(this._text)); stream._close(); }
      return stream;
    }
    set body(_) {}
    _chunk(latin1) {
      if (!this._stream) return;
      const bytes = new Uint8Array(latin1.length);
      for (let i = 0; i < latin1.length; i++) bytes[i] = latin1.charCodeAt(i);
      this._delivered = (this._delivered || 0) + bytes.length;
      this._stream._push(bytes);
    }
    _finish(text) {
      this._finished = true;
      this._text = text;
      if (this._stream) {
        if (!this._delivered && text) this._stream._push(new TextEncoder().encode(text));
        this._stream._close();
      }
      if (this._resolveFull) this._resolveFull(text);
    }
    _fail(e) {
      this._finished = true;
      if (this._stream) this._stream._fail(e);
      if (this._rejectFull) this._rejectFull(e);
    }
    text() { return this._consume(); }
    json() { return this._consume().then((t) => JSON.parse(t)); }
    arrayBuffer() { return this._consume().then((t) => new TextEncoder().encode(t).buffer); }
    blob() { return this._consume().then((t) => new Blob([t], { type: this.headers.get("content-type") || "" })); }
    formData() { return this._consume().then((t) => { const f = new FormData(); new URLSearchParams(t).forEach((v, k) => f.append(k, v)); return f; }); }
    clone() {
      if (this.bodyUsed) throw new TypeError("Response.clone: Body has already been consumed.");
      const r = new Response(this._text, this);
      Object.assign(r, { type: this.type, url: this.url, redirected: this.redirected });
      // A body still arriving: both read it when it's there.
      if (this._full) r._full = this._full;
      return r;
    }
    static json(data, init) { const r = new Response(JSON.stringify(data), init); r.headers.set("content-type", "application/json"); return r; }
    static error() { const r = new Response(null, { status: 0 }); r.type = "error"; return r; }
    static redirect(url, status) { return new Response(null, { status: status || 302, headers: { location: url } }); }
  }

  class AbortSignal extends EventTarget {
    constructor() { super(); this.aborted = false; this.reason = undefined; this.onabort = null; }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    static timeout(ms) { const c = new AbortController(); setTimeout(() => c.abort(new DOMException("The operation timed out.", "TimeoutError")), ms); return c.signal; }
    static abort(reason) { const c = new AbortController(); c.abort(reason); return c.signal; }
  }
  class AbortController {
    constructor() { this.signal = new AbortSignal(); }
    abort(reason) {
      const s = this.signal;
      if (s.aborted) return;
      s.aborted = true;
      s.reason = reason === undefined ? new DOMException("signal is aborted without reason", "AbortError") : reason;
      s.dispatchEvent(new Event("abort"));
      if (typeof s.onabort === "function") s.onabort(new Event("abort"));
    }
  }
  class DOMException extends Error {
    constructor(message, name) { super(message); this.name = name || "Error"; }
  }

  // Requests in flight: id → callback(error, response fields).
  const requests = new Map();
  let nextRequest = 1;
  function toResponse(args) {
    const [, , status, statusText, url, headers, body, type, redirected] = args;
    const r = new Response(body, { status, statusText, headers: new Headers(headers) });
    r.url = url; r.type = type; r.redirected = redirected;
    return r;
  }
  function fetch(input, init) {
    let req;
    try { req = new Request(input, init); } catch (e) { return Promise.reject(e); }
    if (req.signal && req.signal.aborted) return Promise.reject(req.signal.reason);
    const [body, type] = requestBody(req._body);
    if (type && !req.headers.has("content-type")) req.headers.set("content-type", type);
    return new Promise((resolve, reject) => {
      const id = nextRequest++;
      let response = null;
      const failed = (why) => { N.log("warn", "fetch " + req.url + ": " + why); return new TypeError("Failed to fetch"); };
      // The promise settles with the head; the body follows.
      const start = (args) => {
        response = toResponse(args);
        response._text = "";
        response._id = id;
        response._full = new Promise((res, rej) => { response._resolveFull = res; response._rejectFull = rej; });
        response._full.catch(() => {});
        resolve(response);
      };
      requests.set(id, {
        head: start,
        chunk: (data) => { if (response) response._chunk(data); },
        done: (args) => {
          if (args[1] !== null) { const e = failed(args[1]); if (response) response._fail(e); else reject(e); return; }
          if (!response) start(args);
          response._finish(args[6]);
        },
      });
      if (req.signal) req.signal.addEventListener("abort", () => {
        if (!requests.delete(id)) return;
        if (response) response._fail(req.signal.reason); else reject(req.signal.reason);
      });
      N.fetch(id, req.method, req.url, req.headers._list(), body, req.mode, req.credentials);
    });
  }

  class XMLHttpRequest extends EventTarget {
    constructor() {
      super();
      this.readyState = 0; this.status = 0; this.statusText = ""; this.responseText = ""; this.responseURL = "";
      this.responseType = ""; this.timeout = 0; this.withCredentials = false; this.upload = new EventTarget();
      this._headers = new Headers(); this._response = null; this._id = 0;
    }
    get response() {
      if (this.readyState !== 4) return this.responseType === "" || this.responseType === "text" ? "" : null;
      if (this.responseType === "json") { try { return JSON.parse(this.responseText); } catch (_) { return null; } }
      if (this.responseType === "arraybuffer") return new TextEncoder().encode(this.responseText).buffer;
      if (this.responseType === "blob") return new Blob([this.responseText]);
      if (this.responseType === "document") return new DOMParser().parseFromString(this.responseText, "text/html");
      return this.responseText;
    }
    get responseXML() { return this.responseType === "document" ? this.response : null; }
    open(method, url, async) {
      this._method = String(method).toUpperCase();
      this._url = N.resolve(String(url)) || String(url);
      this._async = async !== false;
      this._headers = new Headers();
      this._set(1);
    }
    setRequestHeader(k, v) { this._headers.append(k, v); }
    overrideMimeType() {}
    getResponseHeader(k) { return this._response ? this._response.headers.get(k) : null; }
    getAllResponseHeaders() {
      if (!this._response) return "";
      return this._response.headers._list().map(([k, v]) => k + ": " + v).join("\r\n") + "\r\n";
    }
    // (`onload` and friends run from dispatchEvent.)
    _set(state) {
      this.readyState = state;
      this.dispatchEvent(new Event("readystatechange"));
    }
    _fire(type) { this.dispatchEvent(new Event(type)); }
    _finish(args) {
      if (args[1] !== null) {
        N.log("warn", "XMLHttpRequest " + this._url + ": " + args[1]);
        this.status = 0;
        this._set(4);
        this._fire("error");
        this._fire("loadend");
        return;
      }
      const r = toResponse(args);
      this._response = r;
      this.status = r.status; this.statusText = r.statusText; this.responseURL = r.url; this.responseText = r._text;
      this._set(2); this._set(3); this._set(4);
      this._fire("load");
      this._fire("loadend");
    }
    send(body) {
      const [text, type] = this._method === "GET" || this._method === "HEAD" ? [null, null] : requestBody(body);
      if (type && !this._headers.has("content-type")) this._headers.set("content-type", type);
      const credentials = this.withCredentials ? "include" : "same-origin";
      this._fire("loadstart");
      if (!this._async) {
        this._finish(N.fetchSync(0, this._method, this._url, this._headers._list(), text, "cors", credentials));
        return;
      }
      const id = nextRequest++;
      this._id = id;
      requests.set(id, { head: () => {}, chunk: () => {}, done: (args) => this._finish(args) });
      N.fetch(id, this._method, this._url, this._headers._list(), text, "cors", credentials);
    }
    abort() {
      if (requests.delete(this._id)) { this.readyState = 0; this._fire("abort"); this._fire("loadend"); }
    }
  }
  Object.assign(XMLHttpRequest, { UNSENT: 0, OPENED: 1, HEADERS_RECEIVED: 2, LOADING: 3, DONE: 4 });

  const sockets = new Map();
  let nextSocket = 1;
  const latin1 = (bytes) => { let s = ""; for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]); return s; };
  class MessageEvent extends Event {
    constructor(type, init) { super(type, init); init = init || {}; this.data = init.data; this.origin = init.origin || ""; this.lastEventId = ""; this.source = null; this.ports = []; }
  }
  // MessageChannel: two entangled ports; messages arrive as tasks (React's
  // scheduler and YouTube use them to yield).
  class MessagePort extends EventTarget {
    constructor() { super(); this._other = null; this._started = false; this._queue = []; this.onmessage = null; }
    postMessage(data) {
      const other = this._other;
      if (!other) return;
      setTimeout(() => {
        // (dispatchEvent runs onmessage too.)
        other.dispatchEvent(new MessageEvent("message", { data, source: null }));
      }, 0);
    }
    start() { this._started = true; }
    close() { this._other = null; }
  }
  class MessageChannel {
    constructor() {
      this.port1 = new MessagePort();
      this.port2 = new MessagePort();
      this.port1._other = this.port2;
      this.port2._other = this.port1;
    }
  }
  Object.assign(window, { MessageChannel, MessagePort });
  class CloseEvent extends Event {
    constructor(type, init) { super(type, init); init = init || {}; this.code = init.code || 0; this.reason = init.reason || ""; this.wasClean = !!init.wasClean; }
  }
  class WebSocket extends EventTarget {
    constructor(url, protocols) {
      super();
      this._id = nextSocket++;
      this.readyState = 0;
      this.protocol = "";
      this.extensions = "";
      this.bufferedAmount = 0;
      this.binaryType = "blob";
      const list = protocols === undefined ? [] : [].concat(protocols).map(String);
      this.url = N.wsOpen(this._id, String(url), list);
      sockets.set(this._id, this);
    }
    send(data) {
      if (this.readyState === 0) throw new DOMException("Failed to execute 'send' on 'WebSocket': Still in CONNECTING state.", "InvalidStateError");
      if (this.readyState !== 1) return;
      if (typeof data === "string") N.wsSend(this._id, data, false);
      else if (data instanceof Blob) N.wsSend(this._id, latin1(new TextEncoder().encode(data._text)), true);
      else if (data instanceof ArrayBuffer) N.wsSend(this._id, latin1(new Uint8Array(data)), true);
      else if (ArrayBuffer.isView(data)) N.wsSend(this._id, latin1(new Uint8Array(data.buffer, data.byteOffset, data.byteLength)), true);
      else N.wsSend(this._id, String(data), false);
    }
    close(code, reason) {
      if (this.readyState >= 2) return;
      this.readyState = 2;
      N.wsClose(this._id, code === undefined ? 1000 : code, reason === undefined ? "" : String(reason));
    }
  }
  Object.assign(WebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
  Object.assign(WebSocket.prototype, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
  const eventSources = new Map();
  let nextEventSource = 1;
  class EventSource extends EventTarget {
    constructor(url, init) {
      super();
      this.withCredentials = !!(init && init.withCredentials);
      this.readyState = 0;
      this._id = nextEventSource++;
      eventSources.set(this._id, this);
      this.url = N.sseOpen(this._id, String(url), this.withCredentials);
    }
    close() { this.readyState = 2; eventSources.delete(this._id); N.sseClose(this._id); }
  }
  Object.assign(EventSource, { CONNECTING: 0, OPEN: 1, CLOSED: 2 });
  function sseEvent(id, kind, name, data, lastId) {
    const es = eventSources.get(id);
    if (!es) return;
    if (kind === "open") { es.readyState = 1; es.dispatchEvent(new Event("open")); }
    else if (kind === "message") { const ev = new MessageEvent(name, { data, origin: new URL(es.url).origin }); ev.lastEventId = lastId; es.dispatchEvent(ev); }
    else if (kind === "error") { es.readyState = 0; es.dispatchEvent(new Event("error")); }
    else if (kind === "close") { es.readyState = 2; eventSources.delete(id); es.dispatchEvent(new Event("error")); }
  }
  function socketEvent(id, kind, data, code, reason, clean) {
    const ws = sockets.get(id);
    if (!ws) return;
    if (kind === "open") { ws.readyState = 1; ws.protocol = data; ws.dispatchEvent(new Event("open")); }
    else if (kind === "text") ws.dispatchEvent(new MessageEvent("message", { data }));
    else if (kind === "binary") {
      const bytes = new Uint8Array(data.length);
      for (let i = 0; i < data.length; i++) bytes[i] = data.charCodeAt(i);
      const payload = ws.binaryType === "arraybuffer" ? bytes.buffer : new Blob([new TextDecoder().decode(bytes)]);
      ws.dispatchEvent(new MessageEvent("message", { data: payload }));
    } else if (kind === "error") ws.dispatchEvent(new Event("error"));
    else if (kind === "close") {
      ws.readyState = 3;
      sockets.delete(id);
      ws.dispatchEvent(new CloseEvent("close", { code, reason, wasClean: clean }));
    }
  }

  Object.assign(window, {
    WebSocket, MessageEvent, CloseEvent, __socketEvent: socketEvent,
    fetch, Headers, Request, Response, XMLHttpRequest, FormData, Blob, File, TextEncoder, TextDecoder,
    AbortController, AbortSignal, DOMException,
    __fetchHead(...args) { const r = requests.get(args[0]); if (r) r.head(args); },
    __fetchChunk(id, data) { const r = requests.get(id); if (r) r.chunk(data); },
    __fetchDone(...args) {
      const r = requests.get(args[0]);
      if (!r) return;
      requests.delete(args[0]);
      r.done(args);
    },
    ReadableStream, WritableStream, TransformStream, TextDecoderStream, TextEncoderStream, EventSource, __sseEvent: sseEvent,
  });

  // ---- hooks for the host ----------------------------------------------

  // ---- more of the platform pages expect ---------------------------------

  class DOMRectReadOnly {
    constructor(x = 0, y = 0, width = 0, height = 0) { Object.assign(this, { x, y, width, height }); }
    get top() { return Math.min(this.y, this.y + this.height); }
    get left() { return Math.min(this.x, this.x + this.width); }
    get right() { return Math.max(this.x, this.x + this.width); }
    get bottom() { return Math.max(this.y, this.y + this.height); }
    toJSON() { return { x: this.x, y: this.y, width: this.width, height: this.height, top: this.top, left: this.left, right: this.right, bottom: this.bottom }; }
    static fromRect(r = {}) { return new this(r.x, r.y, r.width, r.height); }
  }
  class DOMRect extends DOMRectReadOnly {}
  class DOMPoint {
    constructor(x = 0, y = 0, z = 0, w = 1) { Object.assign(this, { x, y, z, w }); }
    static fromPoint(p = {}) { return new this(p.x, p.y, p.z, p.w); }
    matrixTransform(m) { return new DOMPoint(m.a * this.x + m.c * this.y + m.e, m.b * this.x + m.d * this.y + m.f); }
  }
  // 2D only.
  class DOMMatrix {
    constructor(init) {
      let v = [1, 0, 0, 1, 0, 0];
      if (Array.isArray(init) && init.length >= 6) v = init.length === 16 ? [init[0], init[1], init[4], init[5], init[12], init[13]] : init.slice(0, 6);
      else if (typeof init === "string") { const m = /matrix\(([^)]*)\)/.exec(init); if (m) v = m[1].split(",").map(Number); }
      [this.a, this.b, this.c, this.d, this.e, this.f] = v;
      this.is2D = true;
    }
    get m11() { return this.a; } get m12() { return this.b; } get m21() { return this.c; } get m22() { return this.d; } get m41() { return this.e; } get m42() { return this.f; }
    get isIdentity() { return this.a === 1 && this.b === 0 && this.c === 0 && this.d === 1 && this.e === 0 && this.f === 0; }
    multiply(o) { const m = this; return new DOMMatrix([m.a * o.a + m.c * o.b, m.b * o.a + m.d * o.b, m.a * o.c + m.c * o.d, m.b * o.c + m.d * o.d, m.a * o.e + m.c * o.f + m.e, m.b * o.e + m.d * o.f + m.f]); }
    translate(x = 0, y = 0) { return this.multiply(new DOMMatrix([1, 0, 0, 1, x, y])); }
    scale(x = 1, y = x) { return this.multiply(new DOMMatrix([x, 0, 0, y, 0, 0])); }
    rotate(deg = 0) { const r = deg * Math.PI / 180, c = Math.cos(r), s = Math.sin(r); return this.multiply(new DOMMatrix([c, s, -s, c, 0, 0])); }
    inverse() { const { a, b, c, d, e, f } = this; const det = a * d - b * c || 1; return new DOMMatrix([d / det, -b / det, -c / det, a / det, (c * f - d * e) / det, (b * e - a * f) / det]); }
    transformPoint(p = {}) { return new DOMPoint(p.x || 0, p.y || 0).matrixTransform(this); }
    toString() { return `matrix(${this.a}, ${this.b}, ${this.c}, ${this.d}, ${this.e}, ${this.f})`; }
  }

  // Ranges: positions in the tree (no layout of their own).
  class StaticRange {
    constructor(init = {}) { Object.assign(this, { startContainer: init.startContainer, startOffset: init.startOffset || 0, endContainer: init.endContainer, endOffset: init.endOffset || 0 }); }
    get collapsed() { return this.startContainer === this.endContainer && this.startOffset === this.endOffset; }
  }
  class Range {
    constructor() { this.startContainer = this.endContainer = document; this.startOffset = this.endOffset = 0; }
    get collapsed() { return this.startContainer === this.endContainer && this.startOffset === this.endOffset; }
    get commonAncestorContainer() {
      const ancestors = new Set();
      for (let n = this.startContainer; n; n = n.parentNode) ancestors.add(n);
      for (let n = this.endContainer; n; n = n.parentNode) if (ancestors.has(n)) return n;
      return document;
    }
    setStart(n, o) { this.startContainer = n; this.startOffset = o; }
    setEnd(n, o) { this.endContainer = n; this.endOffset = o; }
    setStartBefore(n) { this.setStart(n.parentNode, Array.prototype.indexOf.call(n.parentNode.childNodes, n)); }
    setStartAfter(n) { this.setStart(n.parentNode, Array.prototype.indexOf.call(n.parentNode.childNodes, n) + 1); }
    setEndBefore(n) { this.setEnd(n.parentNode, Array.prototype.indexOf.call(n.parentNode.childNodes, n)); }
    setEndAfter(n) { this.setEnd(n.parentNode, Array.prototype.indexOf.call(n.parentNode.childNodes, n) + 1); }
    selectNode(n) { this.setStartBefore(n); this.setEndAfter(n); }
    selectNodeContents(n) { this.setStart(n, 0); this.setEnd(n, n.nodeType === 3 ? n.data.length : n.childNodes.length); }
    collapse(toStart) { if (toStart) this.setEnd(this.startContainer, this.startOffset); else this.setStart(this.endContainer, this.endOffset); }
    cloneRange() { const r = new Range(); r.setStart(this.startContainer, this.startOffset); r.setEnd(this.endContainer, this.endOffset); return r; }
    insertNode(node) {
      const c = this.startContainer;
      if (c.nodeType === 3) { const rest = c.splitText ? c.splitText(this.startOffset) : null; c.parentNode.insertBefore(node, rest || c.nextSibling); }
      else c.insertBefore(node, c.childNodes[this.startOffset] || null);
    }
    createContextualFragment(html) { return wrap(N.parseHTML("div", String(html))); }
    getBoundingClientRect() { const el = this.startContainer && this.startContainer.nodeType === 1 ? this.startContainer : this.startContainer && this.startContainer.parentNode; return el && el.getBoundingClientRect ? el.getBoundingClientRect() : new DOMRect(); }
    getClientRects() { return [this.getBoundingClientRect()]; }
    toString() { return this.startContainer === this.endContainer && this.startContainer.nodeType === 3 ? this.startContainer.data.slice(this.startOffset, this.endOffset) : ""; }
    deleteContents() {} extractContents() { return document.createDocumentFragment(); } cloneContents() { return document.createDocumentFragment(); }
    surroundContents(n) { this.insertNode(n); } detach() {}
    compareBoundaryPoints() { return 0; }
  }
  class Selection {}
  Object.setPrototypeOf(selection, Selection.prototype);
  document.createRange = () => new Range();

  class XMLSerializer {
    serializeToString(n) { return n.nodeType === 9 ? n.documentElement.outerHTML : n.nodeType === 3 ? n.data : n.outerHTML || ""; }
  }

  // Path2D: recorded, replayed by fill(path) / stroke(path) / clip(path).
  class Path2D {
    constructor(other) { this._ops = other instanceof Path2D ? other._ops.slice() : []; }
    addPath(p) { if (p instanceof Path2D) this._ops.push(...p._ops); }
  }
  for (const name of ["moveTo", "lineTo", "closePath", "quadraticCurveTo", "bezierCurveTo", "arc", "arcTo", "ellipse", "rect", "roundRect"]) {
    Path2D.prototype[name] = function (...a) { this._ops.push([name, a]); };
  }

  const cssEscapeImpl = (v) => String(v).replace(/[\0-\x1f\x7f]|^-?\d|^-$|[^\w-]/g, (c, i) => (/[\0-\x1f\x7f]|\d/.test(c) && (i === 0 || /^-/.test(String(v))) ? "\\" + c.charCodeAt(0).toString(16) + " " : "\\" + c));
  const CSSApi = {
    supports(prop, value) { return value === undefined ? N.cssSupports(String(prop)) : N.cssSupports(String(prop), String(value)); },
    escape: cssEscapeImpl,
    registerProperty() {},
  };
  class CSSStyleDeclaration {}

  // Deep copies (the structured clone algorithm, minus transfer).
  function structuredCloneImpl(value, seen = new Map()) {
    if (value === null || typeof value !== "object") {
      if (typeof value === "function" || typeof value === "symbol") throw new DOMException("could not be cloned", "DataCloneError");
      return value;
    }
    if (seen.has(value)) return seen.get(value);
    let out;
    if (value instanceof Date) out = new Date(value.getTime());
    else if (value instanceof RegExp) out = new RegExp(value.source, value.flags);
    else if (value instanceof Map) { out = new Map(); seen.set(value, out); for (const [k, v] of value) out.set(structuredCloneImpl(k, seen), structuredCloneImpl(v, seen)); return out; }
    else if (value instanceof Set) { out = new Set(); seen.set(value, out); for (const v of value) out.add(structuredCloneImpl(v, seen)); return out; }
    else if (ArrayBuffer.isView(value)) out = new value.constructor(value);
    else if (value instanceof ArrayBuffer) out = value.slice(0);
    else if (typeof Blob !== "undefined" && value instanceof Blob) out = value;
    else if (Array.isArray(value)) { out = []; seen.set(value, out); value.forEach((v, i) => { out[i] = structuredCloneImpl(v, seen); }); return out; }
    else { out = {}; seen.set(value, out); for (const k of Object.keys(value)) out[k] = structuredCloneImpl(value[k], seen); return out; }
    seen.set(value, out);
    return out;
  }

  // Other tabs aren't ours to talk to: a channel that hears nothing.
  class BroadcastChannel extends EventTarget {
    constructor(name) { super(); this.name = String(name); this.onmessage = null; }
    postMessage() {} close() {}
  }

  class FileReader extends EventTarget {
    constructor() { super(); this.readyState = 0; this.result = null; this.error = null; }
    _read(blob, how) {
      this.readyState = 1;
      Promise.resolve(blob.arrayBuffer ? blob.arrayBuffer() : new ArrayBuffer(0)).then((buf) => {
        const bytes = new Uint8Array(buf);
        if (how === "text") this.result = new TextDecoder().decode(bytes);
        else if (how === "url") { let bin = ""; for (const b of bytes) bin += String.fromCharCode(b); this.result = "data:" + (blob.type || "application/octet-stream") + ";base64," + btoa(bin); }
        else if (how === "binary") { let bin = ""; for (const b of bytes) bin += String.fromCharCode(b); this.result = bin; }
        else this.result = buf;
        this.readyState = 2;
        for (const type of ["load", "loadend"]) { const ev = new Event(type); if (typeof this["on" + type] === "function") this["on" + type](ev); this.dispatchEvent(ev); }
      });
    }
    readAsText(b) { this._read(b, "text"); } readAsDataURL(b) { this._read(b, "url"); }
    readAsArrayBuffer(b) { this._read(b, "buffer"); } readAsBinaryString(b) { this._read(b, "binary"); }
    abort() {}
  }

  const eventWith = (name, fields, base = Event) => ({ [name]: class extends base {
    constructor(type, init) { super(type, init); init = init || {}; for (const [k, d] of Object.entries(fields)) this[k] = init[k] !== undefined ? init[k] : d; }
  } })[name];
  const events = {
    WheelEvent: eventWith("WheelEvent", { deltaX: 0, deltaY: 0, deltaZ: 0, deltaMode: 0 }, MouseEvent),
    TouchEvent: eventWith("TouchEvent", { touches: [], targetTouches: [], changedTouches: [] }, UIEvent),
    DragEvent: eventWith("DragEvent", { dataTransfer: null }, MouseEvent),
    ClipboardEvent: eventWith("ClipboardEvent", { clipboardData: null }),
    TransitionEvent: eventWith("TransitionEvent", { propertyName: "", elapsedTime: 0, pseudoElement: "" }),
    AnimationEvent: eventWith("AnimationEvent", { animationName: "", elapsedTime: 0, pseudoElement: "" }),
    ErrorEvent: eventWith("ErrorEvent", { message: "", filename: "", lineno: 0, colno: 0, error: null }),
    PromiseRejectionEvent: eventWith("PromiseRejectionEvent", { promise: null, reason: undefined }),
    PageTransitionEvent: eventWith("PageTransitionEvent", { persisted: false }),
    HashChangeEvent: eventWith("HashChangeEvent", { oldURL: "", newURL: "" }),
    PopStateEvent: eventWith("PopStateEvent", { state: null }),
    StorageEvent: eventWith("StorageEvent", { key: null, oldValue: null, newValue: null, url: "", storageArea: null }),
    ProgressEvent: eventWith("ProgressEvent", { lengthComputable: false, loaded: 0, total: 0 }),
  };

  // Web fonts load in the tab; to scripts they're always ready.
  class FontFace {
    constructor(family, source, descriptors) {
      Object.assign(this, { family: String(family), status: "loaded", style: "normal", weight: "normal", display: "auto" }, descriptors || {});
      this.loaded = Promise.resolve(this);
    }
    load() { return Promise.resolve(this); }
  }
  const fontFaces = new Set();
  const fontFaceSet = Object.assign(new EventTarget(), {
    status: "loaded",
    get size() { return fontFaces.size; },
    check: () => true,
    load: () => Promise.resolve([]),
    add(f) { fontFaces.add(f); return this; },
    delete(f) { return fontFaces.delete(f); },
    has(f) { return fontFaces.has(f); },
    clear() { fontFaces.clear(); },
    forEach(cb) { fontFaces.forEach((f) => cb(f, f, this)); },
    values() { return fontFaces.values(); },
    [Symbol.iterator]() { return fontFaces.values(); },
    onloading: null, onloadingdone: null, onloadingerror: null,
  });
  fontFaceSet.ready = Promise.resolve(fontFaceSet);
  class NamedNodeMap {}

  Object.assign(window, {
    DOMRect, DOMRectReadOnly, DOMPoint, DOMPointReadOnly: DOMPoint, DOMMatrix, DOMMatrixReadOnly: DOMMatrix,
    Range, StaticRange, Selection, XMLSerializer, Path2D, CSS: CSSApi, CSSStyleDeclaration,
    structuredClone: (v) => structuredCloneImpl(v), BroadcastChannel, FileReader, MediaQueryList, NamedNodeMap, FontFace,
    Audio: function Audio(src) { const a = document.createElement("audio"); if (src !== undefined) a.setAttribute("src", src); return a; },
    Option: function Option(text, value, defaultSelected, selected) {
      const o = document.createElement("option");
      if (text !== undefined) o.textContent = String(text);
      if (value !== undefined) o.setAttribute("value", String(value));
      if (defaultSelected || selected) o.setAttribute("selected", "");
      return o;
    },
    Notification: Object.assign(function Notification() { throw new TypeError("Notifications aren't available"); }, { permission: "default", requestPermission: () => Promise.resolve("denied") }),
    visualViewport: Object.assign(new EventTarget(), {
      get width() { return viewport[0]; }, get height() { return viewport[1]; }, scale: 1, offsetLeft: 0, offsetTop: 0, pageLeft: 0, pageTop: 0,
    }),
    ...events,
  });

  // The window's own event methods work without a receiver (a bare
  // `addEventListener(...)` in strict code), as browsers' do.
  for (const name of ["addEventListener", "removeEventListener", "dispatchEvent"]) {
    const method = EventTarget.prototype[name];
    Object.defineProperty(window, name, { configurable: true, writable: true, value: function (...args) { return method.apply(this == null || this === globalThis ? window : this, args); } });
  }

  // Declarative shadow roots (made from <template shadowrootmode>).
  {
    const pairs = N.shadowPairs();
    for (let i = 0; i + 1 < pairs.length; i += 2) {
      const host = wrap(pairs[i]), root = wrap(pairs[i + 1]);
      if (!host || !root) continue;
      root._mode = "open";
      root._declarative = true;
      host._shadow = root;
      shadowRoots.add(root);
    }
  }

  Object.assign(window, {
    __setCurrentScript(id) { currentScript = wrap(id); },
    // [[host id | null, css], ...]: constructed stylesheets adopted by
    // shadow roots (by host) and the document (null). The style engine
    // scopes them to their tree, as it does a shadow tree's <style>s.
    __adoptedSheets() {
      const out = [];
      for (const root of shadowRoots) {
        const host = N.shadowHost(root.__id);
        if (host === null || !N.connected(host)) continue;
        for (const sheet of root.adoptedStyleSheets) if (sheet && !sheet.disabled) out.push([host, sheet._text]);
      }
      for (const sheet of documentSheets) if (sheet && !sheet.disabled) out.push([null, sheet._text]);
      return out;
    },
    // [[<style> id, css], ...]: <style> elements whose rules scripts
    // changed through the CSSOM (what they render instead of their text).
    __cssomStyles() {
      const out = [];
      for (const el of cssomStyles) {
        const sheet = el._sheet;
        if (!sheet || !sheet._cssom || sheet._source !== el.textContent || !N.connected(el.__id)) continue;
        out.push([el.__id, sheet._text]);
      }
      return out;
    },
    __setReadyState(s) { readyState = s; document.dispatchEvent(new Event("readystatechange")); },
    __fireDocument(type) { document.dispatchEvent(new Event(type, { bubbles: true })); },
    __fireWindow(type) { window.dispatchEvent(new Event(type)); },
    __fireNode(id, type) { const n = wrap(id); if (n) n.dispatchEvent(new Event(type)); },
    __fireTimer: fireTimer,
    __imageEvent: imageEvent,
    __layout: layout,
    __measuredIds() { return Array.from(measuredIds); },
    // Elements listening for mouse events (window/document count as the
    // root, which every target is under).
    __mouseIds() {
      const out = [];
      const listens = (t) => t && t._listeners && MOUSE_TYPES.some((type) => t._listeners[type] && t._listeners[type].length);
      for (const id of mouseListeners) {
        const w = wrappers.get(id);
        if (!listens(w)) continue;
        const host = w instanceof ShadowRoot ? N.shadowHost(id) : null;
        out.push(host !== null && host !== undefined ? host : id);
      }
      if (listens(window) || listens(document)) out.push(N.root());
      return out;
    },
    __setFocus(id) { moveFocus(id === null ? null : wrap(id)); },
    __dispatchKey(kind, key, code, id, ctrl, shift, alt, meta) {
      if (id !== null && focusedElement !== wrap(id)) moveFocus(wrap(id));
      const target = (id !== null && wrap(id)) || focusedElement || document.body || document.documentElement;
      if (!target) return;
      const init = { bubbles: true, cancelable: true, composed: true, key, code, ctrlKey: ctrl, shiftKey: shift, altKey: alt, metaKey: meta, repeat: false, location: 0, isComposing: false };
      const ev = new KeyboardEvent(kind, init);
      Object.assign(ev, init, { keyCode: KEY_CODES[key] || (key.length === 1 ? key.toUpperCase().charCodeAt(0) : 0) });
      ev.which = ev.keyCode;
      ev.isTrusted = true;
      target.dispatchEvent(ev);
    },
    // `bubbles` false: a boundary event (`mouseenter`...), whose order and
    // targets the runtime works out; `related`: the element left/entered.
    __dispatchMouse(kind, id, x, y, button, buttons, dx, dy, bubbles = true, related = null) {
      const n = wrap(id);
      if (!n) return;
      const r = layoutRects.get(id);
      const init = {
        bubbles: true, cancelable: true, composed: true, view: window, detail: kind === "dblclick" ? 2 : 1,
        clientX: x, clientY: y, screenX: x, screenY: y, pageX: x, pageY: y,
        offsetX: r ? x - r[0] : x, offsetY: r ? y - r[1] : y, movementX: 0, movementY: 0,
        button, buttons,
      };
      if (related !== null) init.relatedTarget = wrap(related);
      if (!bubbles) { init.bubbles = false; init.cancelable = false; }
      const fire = (type, Kind) => { const ev = new Kind(type, init); Object.assign(ev, init); ev.isTrusted = true; n.dispatchEvent(ev); };
      if (/^(mouse|pointer)(over|out|enter|leave)$/.test(kind)) return fire(kind, MouseEvent);
      if (kind === "wheel") {
        const ev = new MouseEvent("wheel", init);
        Object.assign(ev, init, { deltaX: dx, deltaY: dy, deltaZ: 0, deltaMode: 0 });
        ev.isTrusted = true;
        n.dispatchEvent(ev);
        return;
      }
      const pointer = { mousedown: "pointerdown", mouseup: "pointerup", mousemove: "pointermove" }[kind];
      if (pointer) { fire(pointer, MouseEvent); }
      fire(kind, MouseEvent);
    },
    __flushCanvases: flushCanvases,
    __traverse: traverse,
    __dispatchClick(id) {
      const n = wrap(id);
      if (!n) return false;
      const ev = new MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
      ev.isTrusted = true;
      return !n.dispatchEvent(ev);
    },
    __dispatchChecked(id, checked) {
      const n = wrap(id);
      if (!n) return;
      if (checked) n.setAttribute("checked", ""); else n.removeAttribute("checked");
      n.dispatchEvent(new InputEvent("input", { bubbles: true }));
      n.dispatchEvent(new Event("change", { bubbles: true }));
    },
    __dispatchSubmit(id) {
      const n = wrap(id);
      if (!n) return false;
      const ev = new Event("submit", { bubbles: true, cancelable: true });
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
        if (w && ((w._listeners && w._listeners.click && w._listeners.click.length) || w._onclick)) {
          // The tab sees shadow trees flattened into their hosts.
          const host = w instanceof ShadowRoot ? N.shadowHost(id) : null;
          out.push(host !== null && host !== undefined ? host : id);
        }
      }
      for (const n of N.query(N.root(), "[onclick]", true)) out.push(n);
      return out;
    },
  });
  // Debugging: FEATHER_EVAL=probe.js runs in the page FEATHER_EVAL_DELAY ms in.
  const probe = N.debugScript();
  if (probe) {
    const run = () => { try { (0, eval)(probe[0]); } catch (e) { console.error("FEATHER_EVAL: " + e + "\n" + e.stack); } };
    // A negative delay: before the page's own scripts (to wrap things they use).
    if (probe[1] < 0) run(); else setTimeout(run, probe[1]);
  }
})(globalThis);
