// nub-env.d.ts — ambient declarations for code authored against the Nub runtime.
//
// Project-local fallback, identical in content to the @nubjs/types package. Use
// this when you can't add the package (offline, no install step): keep it in your
// project tree and let tsconfig `include` pick it up. Prefer the package where
// possible (`npm i -D @nubjs/types` + `types: ["node", "@nubjs/types"]`).
//
// Nub augments Node with surfaces TypeScript doesn't know about. This file makes
// that nub-authored code typecheck so the parity bar holds: "if `tsc --noEmit`
// accepts your code, nub runs it."
//
// Only declares surfaces that @types/node does NOT already cover. Everything Nub
// merely flag-enables (URLPattern, WebSocket, EventSource, navigator.locks,
// localStorage/sessionStorage, node:sqlite, Float16Array, RegExp.escape, …) is
// already typed by @types/node + TypeScript's bundled libs and is intentionally
// absent here.
//
// MUST remain a global *script* file: NO top-level `import`/`export`. The wildcard
// `declare module "*.yaml"` declarations are only visible project-wide from a
// script file. (Adding `export {}` turns this into a module and silently breaks
// the data-import wildcards.) Globals are declared bare (`declare function …`,
// `declare var …`, `declare namespace …`) for the same reason.

// ── Data-format module imports (Nub load hook; wiki/runtime/data-loaders.md) ──
// Default export only — a wildcard cannot statically know per-key shapes, so
// named imports (`import { host } from "./c.yaml"`) are not typed (they still RUN
// on nub). `.json` is intentionally NOT declared: it's Node-native (resolveJsonModule).
declare module "*.yaml" {
  const data: unknown;
  export default data;
}
declare module "*.yml" {
  const data: unknown;
  export default data;
}
declare module "*.toml" {
  const data: unknown;
  export default data;
}
declare module "*.jsonc" {
  const data: unknown;
  export default data;
}
declare module "*.json5" {
  const data: unknown;
  export default data;
}
declare module "*.txt" {
  const data: string;
  export default data;
}

// ── reportError (WinterTC min-common-API; runtime/polyfills.cjs) ──
// In no Node version, in no @types/node. Nub installs it on every supported version.
declare function reportError(error: unknown): void;

// ── Browser-shape Worker global (runtime/worker-polyfill.mjs; wiki/runtime/web-worker.md) ──
// Nub ships the WHATWG/browser subset of `Worker` over node:worker_threads.Worker.
// @types/node has NO global `Worker` (only node:worker_threads' class), so this is
// the genuine gap. `MessageEvent`, `ErrorEvent`, and `MessagePort` are ALREADY
// global in @types/node (web-globals/fetch.d.ts + messaging.d.ts) — verified
// empirically — so they are referenced from there and intentionally NOT redeclared
// here (redeclaring them collides: TS2403).
interface WorkerOptions {
  type?: "module" | "classic";
  name?: string;
  credentials?: "omit" | "same-origin" | "include";
}
interface Worker extends EventTarget {
  postMessage(message: any, transfer?: readonly (ArrayBuffer | MessagePort)[]): void;
  terminate(): void;
  onmessage: ((this: Worker, ev: MessageEvent) => any) | null;
  onmessageerror: ((this: Worker, ev: MessageEvent) => any) | null;
  onerror: ((this: Worker, ev: ErrorEvent) => any) | null;
}
declare var Worker: {
  prototype: Worker;
  new (scriptURL: string | URL, options?: WorkerOptions): Worker;
};

// ── import.meta.hot (Vite-compatible; wiki/runtime/hot-mode.md — v0.x, shape committed v0.1) ──
// Forward-compat commitment: ships now so framework authors can code against the
// shape. `import.meta.hot` is `undefined` unless `nub watch --hot` is active.
interface ImportMeta {
  readonly hot?: {
    readonly data: Record<string, any>;
    accept(): void;
    accept(cb: (mod: any) => void): void;
    accept(dep: string, cb: (mod: any) => void): void;
    accept(deps: readonly string[], cb: (mods: any[]) => void): void;
    dispose(cb: (data: Record<string, any>) => void): void;
    invalidate(): void;
    on(event: string, cb: (data: any) => void): void;
    send(event: string, data?: any): void;
  };
}

// ── Date.prototype.toTemporalInstant (runtime/preload-common.cjs installs it) ──
// Nub assigns the polyfill's `toTemporalInstant` onto Date.prototype on the floor
// (matching native Node, which ships it once Temporal is native).
interface Date {
  toTemporalInstant(): Temporal.Instant;
}

// ── Temporal (vendored @js-temporal/polyfill@0.5.1; runtime/preload-common.cjs) ──
// In no Node version, in no @types/node. The namespace below is inlined verbatim
// from @js-temporal/polyfill@0.5.1's index.d.ts (the version Nub bundles), with
// `export` markers stripped so it is an ambient global rather than a module export.
// Keep in sync with the bundled polyfill version on every bump.
declare namespace Temporal {
  type ComparisonResult = -1 | 0 | 1;
  type RoundingMode =
    | "ceil"
    | "floor"
    | "expand"
    | "trunc"
    | "halfCeil"
    | "halfFloor"
    | "halfExpand"
    | "halfTrunc"
    | "halfEven";

  type AssignmentOptions = {
    overflow?: "constrain" | "reject";
  };

  type DurationOptions = {
    overflow?: "constrain" | "balance";
  };

  type ToInstantOptions = {
    disambiguation?: "compatible" | "earlier" | "later" | "reject";
  };

  type OffsetDisambiguationOptions = {
    offset?: "use" | "prefer" | "ignore" | "reject";
  };

  type ZonedDateTimeAssignmentOptions = Partial<
    AssignmentOptions & ToInstantOptions & OffsetDisambiguationOptions
  >;

  type ArithmeticOptions = {
    overflow?: "constrain" | "reject";
  };

  type DateUnit = "year" | "month" | "week" | "day";
  type TimeUnit = "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond";
  type DateTimeUnit = DateUnit | TimeUnit;

  type PluralUnit<T extends DateTimeUnit> = {
    year: "years";
    month: "months";
    week: "weeks";
    day: "days";
    hour: "hours";
    minute: "minutes";
    second: "seconds";
    millisecond: "milliseconds";
    microsecond: "microseconds";
    nanosecond: "nanoseconds";
  }[T];

  type LargestUnit<T extends DateTimeUnit> = "auto" | T | PluralUnit<T>;
  type SmallestUnit<T extends DateTimeUnit> = T | PluralUnit<T>;
  type TotalUnit<T extends DateTimeUnit> = T | PluralUnit<T>;

  type ToStringPrecisionOptions = {
    fractionalSecondDigits?: "auto" | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9;
    smallestUnit?: SmallestUnit<"minute" | "second" | "millisecond" | "microsecond" | "nanosecond">;
    roundingMode?: RoundingMode;
  };

  type ShowCalendarOption = {
    calendarName?: "auto" | "always" | "never" | "critical";
  };

  type CalendarTypeToStringOptions = Partial<ToStringPrecisionOptions & ShowCalendarOption>;

  type ZonedDateTimeToStringOptions = Partial<
    CalendarTypeToStringOptions & {
      timeZoneName?: "auto" | "never" | "critical";
      offset?: "auto" | "never";
    }
  >;

  type InstantToStringOptions = Partial<
    ToStringPrecisionOptions & {
      timeZone: TimeZoneLike;
    }
  >;

  interface DifferenceOptions<T extends DateTimeUnit> {
    smallestUnit?: SmallestUnit<T>;
    largestUnit?: LargestUnit<T>;
    roundingIncrement?: number;
    roundingMode?: RoundingMode;
  }

  type RoundTo<T extends DateTimeUnit> =
    | SmallestUnit<T>
    | {
        smallestUnit: SmallestUnit<T>;
        roundingIncrement?: number;
        roundingMode?: RoundingMode;
      };

  type DurationRoundTo =
    | SmallestUnit<DateTimeUnit>
    | ((
        | {
            smallestUnit: SmallestUnit<DateTimeUnit>;
            largestUnit?: LargestUnit<DateTimeUnit>;
          }
        | {
            smallestUnit?: SmallestUnit<DateTimeUnit>;
            largestUnit: LargestUnit<DateTimeUnit>;
          }
      ) & {
        roundingIncrement?: number;
        roundingMode?: RoundingMode;
        relativeTo?:
          | Temporal.PlainDateTime
          | Temporal.ZonedDateTime
          | PlainDateTimeLike
          | ZonedDateTimeLike
          | string;
      });

  type DurationTotalOf =
    | TotalUnit<DateTimeUnit>
    | {
        unit: TotalUnit<DateTimeUnit>;
        relativeTo?:
          | Temporal.ZonedDateTime
          | Temporal.PlainDateTime
          | ZonedDateTimeLike
          | PlainDateTimeLike
          | string;
      };

  interface DurationArithmeticOptions {
    relativeTo?:
      | Temporal.ZonedDateTime
      | Temporal.PlainDateTime
      | ZonedDateTimeLike
      | PlainDateTimeLike
      | string;
  }

  type TransitionDirection = "next" | "previous" | { direction: "next" | "previous" };

  type LocalesArgument = ConstructorParameters<typeof Intl.DateTimeFormat>[0];
  type DurationFormatOptions = typeof Intl extends { DurationFormat: any }
    ? ConstructorParameters<(typeof Intl)["DurationFormat"]>[1]
    : Record<string, unknown>;

  type DurationLike = {
    years?: number;
    months?: number;
    weeks?: number;
    days?: number;
    hours?: number;
    minutes?: number;
    seconds?: number;
    milliseconds?: number;
    microseconds?: number;
    nanoseconds?: number;
  };

  class Duration {
    static from(item: Temporal.Duration | DurationLike | string): Temporal.Duration;
    static compare(
      one: Temporal.Duration | DurationLike | string,
      two: Temporal.Duration | DurationLike | string,
      options?: DurationArithmeticOptions
    ): ComparisonResult;
    constructor(
      years?: number,
      months?: number,
      weeks?: number,
      days?: number,
      hours?: number,
      minutes?: number,
      seconds?: number,
      milliseconds?: number,
      microseconds?: number,
      nanoseconds?: number
    );
    readonly sign: -1 | 0 | 1;
    readonly blank: boolean;
    readonly years: number;
    readonly months: number;
    readonly weeks: number;
    readonly days: number;
    readonly hours: number;
    readonly minutes: number;
    readonly seconds: number;
    readonly milliseconds: number;
    readonly microseconds: number;
    readonly nanoseconds: number;
    negated(): Temporal.Duration;
    abs(): Temporal.Duration;
    with(durationLike: DurationLike): Temporal.Duration;
    add(other: Temporal.Duration | DurationLike | string): Temporal.Duration;
    subtract(other: Temporal.Duration | DurationLike | string): Temporal.Duration;
    round(roundTo: DurationRoundTo): Temporal.Duration;
    total(totalOf: DurationTotalOf): number;
    toLocaleString(locales?: LocalesArgument, options?: DurationFormatOptions): string;
    toJSON(): string;
    toString(options?: ToStringPrecisionOptions): string;
    valueOf(): never;
    readonly [Symbol.toStringTag]: "Temporal.Duration";
  }

  class Instant {
    static fromEpochMilliseconds(epochMilliseconds: number): Temporal.Instant;
    static fromEpochNanoseconds(epochNanoseconds: bigint): Temporal.Instant;
    static from(item: Temporal.Instant | string): Temporal.Instant;
    static compare(one: Temporal.Instant | string, two: Temporal.Instant | string): ComparisonResult;
    constructor(epochNanoseconds: bigint);
    readonly epochMilliseconds: number;
    readonly epochNanoseconds: bigint;
    equals(other: Temporal.Instant | string): boolean;
    add(
      durationLike: Omit<Temporal.Duration | DurationLike, "years" | "months" | "weeks" | "days"> | string
    ): Temporal.Instant;
    subtract(
      durationLike: Omit<Temporal.Duration | DurationLike, "years" | "months" | "weeks" | "days"> | string
    ): Temporal.Instant;
    until(
      other: Temporal.Instant | string,
      options?: DifferenceOptions<"hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond">
    ): Temporal.Duration;
    since(
      other: Temporal.Instant | string,
      options?: DifferenceOptions<"hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond">
    ): Temporal.Duration;
    round(
      roundTo: RoundTo<"hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond">
    ): Temporal.Instant;
    toZonedDateTimeISO(tzLike: TimeZoneLike): Temporal.ZonedDateTime;
    toLocaleString(locales?: LocalesArgument, options?: globalThis.Intl.DateTimeFormatOptions): string;
    toJSON(): string;
    toString(options?: InstantToStringOptions): string;
    valueOf(): never;
    readonly [Symbol.toStringTag]: "Temporal.Instant";
  }

  type CalendarLike = string | ZonedDateTime | PlainDateTime | PlainDate | PlainYearMonth | PlainMonthDay;

  type PlainDateLike = {
    era?: string | undefined;
    eraYear?: number | undefined;
    year?: number;
    month?: number;
    monthCode?: string;
    day?: number;
    calendar?: CalendarLike;
  };

  class PlainDate {
    static from(item: Temporal.PlainDate | PlainDateLike | string, options?: AssignmentOptions): Temporal.PlainDate;
    static compare(
      one: Temporal.PlainDate | PlainDateLike | string,
      two: Temporal.PlainDate | PlainDateLike | string
    ): ComparisonResult;
    constructor(isoYear: number, isoMonth: number, isoDay: number, calendar?: string);
    readonly era: string | undefined;
    readonly eraYear: number | undefined;
    readonly year: number;
    readonly month: number;
    readonly monthCode: string;
    readonly day: number;
    readonly calendarId: string;
    readonly dayOfWeek: number;
    readonly dayOfYear: number;
    readonly weekOfYear: number | undefined;
    readonly yearOfWeek: number | undefined;
    readonly daysInWeek: number;
    readonly daysInYear: number;
    readonly daysInMonth: number;
    readonly monthsInYear: number;
    readonly inLeapYear: boolean;
    equals(other: Temporal.PlainDate | PlainDateLike | string): boolean;
    with(dateLike: PlainDateLike, options?: AssignmentOptions): Temporal.PlainDate;
    withCalendar(calendar: CalendarLike): Temporal.PlainDate;
    add(durationLike: Temporal.Duration | DurationLike | string, options?: ArithmeticOptions): Temporal.PlainDate;
    subtract(durationLike: Temporal.Duration | DurationLike | string, options?: ArithmeticOptions): Temporal.PlainDate;
    until(
      other: Temporal.PlainDate | PlainDateLike | string,
      options?: DifferenceOptions<"year" | "month" | "week" | "day">
    ): Temporal.Duration;
    since(
      other: Temporal.PlainDate | PlainDateLike | string,
      options?: DifferenceOptions<"year" | "month" | "week" | "day">
    ): Temporal.Duration;
    toPlainDateTime(temporalTime?: Temporal.PlainTime | PlainTimeLike | string): Temporal.PlainDateTime;
    toZonedDateTime(
      timeZoneAndTime:
        | string
        | {
            timeZone: TimeZoneLike;
            plainTime?: Temporal.PlainTime | PlainTimeLike | string;
          }
    ): Temporal.ZonedDateTime;
    toPlainYearMonth(): Temporal.PlainYearMonth;
    toPlainMonthDay(): Temporal.PlainMonthDay;
    toLocaleString(locales?: LocalesArgument, options?: globalThis.Intl.DateTimeFormatOptions): string;
    toJSON(): string;
    toString(options?: ShowCalendarOption): string;
    valueOf(): never;
    readonly [Symbol.toStringTag]: "Temporal.PlainDate";
  }

  type PlainDateTimeLike = {
    era?: string | undefined;
    eraYear?: number | undefined;
    year?: number;
    month?: number;
    monthCode?: string;
    day?: number;
    hour?: number;
    minute?: number;
    second?: number;
    millisecond?: number;
    microsecond?: number;
    nanosecond?: number;
    calendar?: CalendarLike;
  };

  class PlainDateTime {
    static from(
      item: Temporal.PlainDateTime | PlainDateTimeLike | string,
      options?: AssignmentOptions
    ): Temporal.PlainDateTime;
    static compare(
      one: Temporal.PlainDateTime | PlainDateTimeLike | string,
      two: Temporal.PlainDateTime | PlainDateTimeLike | string
    ): ComparisonResult;
    constructor(
      isoYear: number,
      isoMonth: number,
      isoDay: number,
      hour?: number,
      minute?: number,
      second?: number,
      millisecond?: number,
      microsecond?: number,
      nanosecond?: number,
      calendar?: string
    );
    readonly era: string | undefined;
    readonly eraYear: number | undefined;
    readonly year: number;
    readonly month: number;
    readonly monthCode: string;
    readonly day: number;
    readonly hour: number;
    readonly minute: number;
    readonly second: number;
    readonly millisecond: number;
    readonly microsecond: number;
    readonly nanosecond: number;
    readonly calendarId: string;
    readonly dayOfWeek: number;
    readonly dayOfYear: number;
    readonly weekOfYear: number | undefined;
    readonly yearOfWeek: number | undefined;
    readonly daysInWeek: number;
    readonly daysInYear: number;
    readonly daysInMonth: number;
    readonly monthsInYear: number;
    readonly inLeapYear: boolean;
    equals(other: Temporal.PlainDateTime | PlainDateTimeLike | string): boolean;
    with(dateTimeLike: PlainDateTimeLike, options?: AssignmentOptions): Temporal.PlainDateTime;
    withPlainTime(timeLike?: Temporal.PlainTime | PlainTimeLike | string): Temporal.PlainDateTime;
    withCalendar(calendar: CalendarLike): Temporal.PlainDateTime;
    add(durationLike: Temporal.Duration | DurationLike | string, options?: ArithmeticOptions): Temporal.PlainDateTime;
    subtract(
      durationLike: Temporal.Duration | DurationLike | string,
      options?: ArithmeticOptions
    ): Temporal.PlainDateTime;
    until(
      other: Temporal.PlainDateTime | PlainDateTimeLike | string,
      options?: DifferenceOptions<
        "year" | "month" | "week" | "day" | "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond"
      >
    ): Temporal.Duration;
    since(
      other: Temporal.PlainDateTime | PlainDateTimeLike | string,
      options?: DifferenceOptions<
        "year" | "month" | "week" | "day" | "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond"
      >
    ): Temporal.Duration;
    round(
      roundTo: RoundTo<"day" | "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond">
    ): Temporal.PlainDateTime;
    toZonedDateTime(tzLike: TimeZoneLike, options?: ToInstantOptions): Temporal.ZonedDateTime;
    toPlainDate(): Temporal.PlainDate;
    toPlainTime(): Temporal.PlainTime;
    toLocaleString(locales?: LocalesArgument, options?: globalThis.Intl.DateTimeFormatOptions): string;
    toJSON(): string;
    toString(options?: CalendarTypeToStringOptions): string;
    valueOf(): never;
    readonly [Symbol.toStringTag]: "Temporal.PlainDateTime";
  }

  type PlainMonthDayLike = {
    era?: string | undefined;
    eraYear?: number | undefined;
    year?: number;
    month?: number;
    monthCode?: string;
    day?: number;
    calendar?: CalendarLike;
  };

  class PlainMonthDay {
    static from(
      item: Temporal.PlainMonthDay | PlainMonthDayLike | string,
      options?: AssignmentOptions
    ): Temporal.PlainMonthDay;
    constructor(isoMonth: number, isoDay: number, calendar?: string, referenceISOYear?: number);
    readonly monthCode: string;
    readonly day: number;
    readonly calendarId: string;
    equals(other: Temporal.PlainMonthDay | PlainMonthDayLike | string): boolean;
    with(monthDayLike: PlainMonthDayLike, options?: AssignmentOptions): Temporal.PlainMonthDay;
    toPlainDate(year: { year: number }): Temporal.PlainDate;
    toLocaleString(locales?: LocalesArgument, options?: globalThis.Intl.DateTimeFormatOptions): string;
    toJSON(): string;
    toString(options?: ShowCalendarOption): string;
    valueOf(): never;
    readonly [Symbol.toStringTag]: "Temporal.PlainMonthDay";
  }

  type PlainTimeLike = {
    hour?: number;
    minute?: number;
    second?: number;
    millisecond?: number;
    microsecond?: number;
    nanosecond?: number;
  };

  class PlainTime {
    static from(item: Temporal.PlainTime | PlainTimeLike | string, options?: AssignmentOptions): Temporal.PlainTime;
    static compare(
      one: Temporal.PlainTime | PlainTimeLike | string,
      two: Temporal.PlainTime | PlainTimeLike | string
    ): ComparisonResult;
    constructor(
      hour?: number,
      minute?: number,
      second?: number,
      millisecond?: number,
      microsecond?: number,
      nanosecond?: number
    );
    readonly hour: number;
    readonly minute: number;
    readonly second: number;
    readonly millisecond: number;
    readonly microsecond: number;
    readonly nanosecond: number;
    equals(other: Temporal.PlainTime | PlainTimeLike | string): boolean;
    with(timeLike: Temporal.PlainTime | PlainTimeLike, options?: AssignmentOptions): Temporal.PlainTime;
    add(durationLike: Temporal.Duration | DurationLike | string, options?: ArithmeticOptions): Temporal.PlainTime;
    subtract(durationLike: Temporal.Duration | DurationLike | string, options?: ArithmeticOptions): Temporal.PlainTime;
    until(
      other: Temporal.PlainTime | PlainTimeLike | string,
      options?: DifferenceOptions<"hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond">
    ): Temporal.Duration;
    since(
      other: Temporal.PlainTime | PlainTimeLike | string,
      options?: DifferenceOptions<"hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond">
    ): Temporal.Duration;
    round(
      roundTo: RoundTo<"hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond">
    ): Temporal.PlainTime;
    toLocaleString(locales?: LocalesArgument, options?: globalThis.Intl.DateTimeFormatOptions): string;
    toJSON(): string;
    toString(options?: ToStringPrecisionOptions): string;
    valueOf(): never;
    readonly [Symbol.toStringTag]: "Temporal.PlainTime";
  }

  type TimeZoneLike = string | ZonedDateTime;

  type PlainYearMonthLike = {
    era?: string | undefined;
    eraYear?: number | undefined;
    year?: number;
    month?: number;
    monthCode?: string;
    calendar?: CalendarLike;
  };

  class PlainYearMonth {
    static from(
      item: Temporal.PlainYearMonth | PlainYearMonthLike | string,
      options?: AssignmentOptions
    ): Temporal.PlainYearMonth;
    static compare(
      one: Temporal.PlainYearMonth | PlainYearMonthLike | string,
      two: Temporal.PlainYearMonth | PlainYearMonthLike | string
    ): ComparisonResult;
    constructor(isoYear: number, isoMonth: number, calendar?: string, referenceISODay?: number);
    readonly era: string | undefined;
    readonly eraYear: number | undefined;
    readonly year: number;
    readonly month: number;
    readonly monthCode: string;
    readonly calendarId: string;
    readonly daysInMonth: number;
    readonly daysInYear: number;
    readonly monthsInYear: number;
    readonly inLeapYear: boolean;
    equals(other: Temporal.PlainYearMonth | PlainYearMonthLike | string): boolean;
    with(yearMonthLike: PlainYearMonthLike, options?: AssignmentOptions): Temporal.PlainYearMonth;
    add(durationLike: Temporal.Duration | DurationLike | string, options?: ArithmeticOptions): Temporal.PlainYearMonth;
    subtract(
      durationLike: Temporal.Duration | DurationLike | string,
      options?: ArithmeticOptions
    ): Temporal.PlainYearMonth;
    until(
      other: Temporal.PlainYearMonth | PlainYearMonthLike | string,
      options?: DifferenceOptions<"year" | "month">
    ): Temporal.Duration;
    since(
      other: Temporal.PlainYearMonth | PlainYearMonthLike | string,
      options?: DifferenceOptions<"year" | "month">
    ): Temporal.Duration;
    toPlainDate(day: { day: number }): Temporal.PlainDate;
    toLocaleString(locales?: LocalesArgument, options?: globalThis.Intl.DateTimeFormatOptions): string;
    toJSON(): string;
    toString(options?: ShowCalendarOption): string;
    valueOf(): never;
    readonly [Symbol.toStringTag]: "Temporal.PlainYearMonth";
  }

  type ZonedDateTimeLike = {
    era?: string | undefined;
    eraYear?: number | undefined;
    year?: number;
    month?: number;
    monthCode?: string;
    day?: number;
    hour?: number;
    minute?: number;
    second?: number;
    millisecond?: number;
    microsecond?: number;
    nanosecond?: number;
    offset?: string;
    timeZone?: TimeZoneLike;
    calendar?: CalendarLike;
  };

  class ZonedDateTime {
    static from(
      item: Temporal.ZonedDateTime | ZonedDateTimeLike | string,
      options?: ZonedDateTimeAssignmentOptions
    ): ZonedDateTime;
    static compare(
      one: Temporal.ZonedDateTime | ZonedDateTimeLike | string,
      two: Temporal.ZonedDateTime | ZonedDateTimeLike | string
    ): ComparisonResult;
    constructor(epochNanoseconds: bigint, timeZone: string, calendar?: string);
    readonly era: string | undefined;
    readonly eraYear: number | undefined;
    readonly year: number;
    readonly month: number;
    readonly monthCode: string;
    readonly day: number;
    readonly hour: number;
    readonly minute: number;
    readonly second: number;
    readonly millisecond: number;
    readonly microsecond: number;
    readonly nanosecond: number;
    readonly timeZoneId: string;
    readonly calendarId: string;
    readonly dayOfWeek: number;
    readonly dayOfYear: number;
    readonly weekOfYear: number | undefined;
    readonly yearOfWeek: number | undefined;
    readonly hoursInDay: number;
    readonly daysInWeek: number;
    readonly daysInMonth: number;
    readonly daysInYear: number;
    readonly monthsInYear: number;
    readonly inLeapYear: boolean;
    readonly offsetNanoseconds: number;
    readonly offset: string;
    readonly epochMilliseconds: number;
    readonly epochNanoseconds: bigint;
    equals(other: Temporal.ZonedDateTime | ZonedDateTimeLike | string): boolean;
    with(zonedDateTimeLike: ZonedDateTimeLike, options?: ZonedDateTimeAssignmentOptions): Temporal.ZonedDateTime;
    withPlainTime(timeLike?: Temporal.PlainTime | PlainTimeLike | string): Temporal.ZonedDateTime;
    withCalendar(calendar: CalendarLike): Temporal.ZonedDateTime;
    withTimeZone(timeZone: TimeZoneLike): Temporal.ZonedDateTime;
    add(durationLike: Temporal.Duration | DurationLike | string, options?: ArithmeticOptions): Temporal.ZonedDateTime;
    subtract(
      durationLike: Temporal.Duration | DurationLike | string,
      options?: ArithmeticOptions
    ): Temporal.ZonedDateTime;
    until(
      other: Temporal.ZonedDateTime | ZonedDateTimeLike | string,
      options?: Temporal.DifferenceOptions<
        "year" | "month" | "week" | "day" | "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond"
      >
    ): Temporal.Duration;
    since(
      other: Temporal.ZonedDateTime | ZonedDateTimeLike | string,
      options?: Temporal.DifferenceOptions<
        "year" | "month" | "week" | "day" | "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond"
      >
    ): Temporal.Duration;
    round(
      roundTo: RoundTo<"day" | "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond">
    ): Temporal.ZonedDateTime;
    startOfDay(): Temporal.ZonedDateTime;
    getTimeZoneTransition(direction: TransitionDirection): Temporal.ZonedDateTime | null;
    toInstant(): Temporal.Instant;
    toPlainDateTime(): Temporal.PlainDateTime;
    toPlainDate(): Temporal.PlainDate;
    toPlainTime(): Temporal.PlainTime;
    toLocaleString(locales?: LocalesArgument, options?: globalThis.Intl.DateTimeFormatOptions): string;
    toJSON(): string;
    toString(options?: ZonedDateTimeToStringOptions): string;
    valueOf(): never;
    readonly [Symbol.toStringTag]: "Temporal.ZonedDateTime";
  }

  const Now: {
    instant: () => Temporal.Instant;
    zonedDateTimeISO: (tzLike?: TimeZoneLike) => Temporal.ZonedDateTime;
    plainDateTimeISO: (tzLike?: TimeZoneLike) => Temporal.PlainDateTime;
    plainDateISO: (tzLike?: TimeZoneLike) => Temporal.PlainDate;
    plainTimeISO: (tzLike?: TimeZoneLike) => Temporal.PlainTime;
    timeZoneId: () => string;
    readonly [Symbol.toStringTag]: "Temporal.Now";
  };
}
// `Temporal` is exposed as an ambient global namespace. It is both a type namespace
// and a value (its `class` and `const` members make it a runtime value), so no
// separate `declare var Temporal` is needed — and adding one collides (TS2300).
