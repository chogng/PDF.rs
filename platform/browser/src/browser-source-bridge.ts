import {
  DataAttachmentRole,
  SourceFailureCode,
  snapshotFailDataCommand,
  snapshotProvideDataCommand,
  snapshotSourceDescriptor,
  snapshotSourceIdentity,
  validateFailDataCommand,
  validateNeedDataEvent,
  validateProvideDataCommand,
  validateProvideDataTransferLengths,
  validateRequestId,
  validateSessionId,
  validateSourceDescriptor,
  validateSourceFailureCode,
  validateSourceIdentity,
  validateWorkerId,
  type ByteRange,
  type DataTicket,
  type FailDataCommand,
  type NeedDataEvent,
  type ProvideDataCommand,
  type SourceDescriptor,
  type SourceIdentity,
} from "../generated/engine-protocol.js";

const MAX_U64 = 0xffff_ffff_ffff_ffffn;
const MAX_SOURCE_CAPACITY = 4_096;
const MAX_SOURCE_BYTES = 1_073_741_824;
const MAX_URL_LENGTH = 8_192;

/** Maximum UTF-16 code units in an equivalent validator header name. */
export const MAX_BROWSER_HTTP_VALIDATOR_HEADER_NAME_LENGTH = 128 as const;

/** Maximum UTF-16 code units in a strong ETag or equivalent value. */
export const MAX_BROWSER_HTTP_VALIDATOR_VALUE_LENGTH = 1_024 as const;

/** Stable bridge construction, lifecycle, and sink failures. */
export type BrowserSourceBridgeErrorCode =
  | "InvalidConfiguration"
  | "InvalidLifecycle"
  | "InvalidTurnBudget"
  | "SinkFailure"
  | "SinkOwnershipFailure";

/** A content-free source bridge API error. */
export class BrowserSourceBridgeError extends Error {
  readonly code: BrowserSourceBridgeErrorCode;

  constructor(code: BrowserSourceBridgeErrorCode) {
    super(code);
    this.name = "BrowserSourceBridgeError";
    this.code = code;
  }
}

/** Independent bounds for acquisition, completion, and retained bytes. */
export interface BrowserSourceBridgeLimits {
  readonly maxActiveTickets: number;
  readonly maxQueuedResults: number;
  readonly maxTrackedTickets: number;
  readonly maxBufferedBytes: number;
  readonly maxWholeSourceBytes: number;
  readonly maxDrainTurn: number;
}

/** Opaque abort handle supplied to every asynchronous acquisition. */
export interface BrowserSourceAbortHandle {
  readonly signal: unknown;
  abort(): void;
}

/** Creates one abort handle per accepted DataTicket. */
export interface BrowserSourceAbortFactory {
  create(): BrowserSourceAbortHandle;
}

/** Stable adapter failure; no exception string is inspected. */
export interface BrowserSourceReadFailure {
  readonly type: "Failure";
  readonly code: SourceFailureCode;
  readonly observed?: SourceIdentity;
}

/** Exact request made to an immutable local-file adapter. */
export interface BrowserLocalReadRequest {
  readonly source: SourceIdentity;
  readonly range: ByteRange;
  readonly maximumBytes: number;
  readonly signal: unknown;
}

/** Exact local-file completion metadata and detached-capable bytes. */
export interface BrowserLocalReadBytes {
  readonly type: "Bytes";
  readonly source: SourceIdentity;
  readonly range: ByteRange;
  readonly totalLength: bigint;
  readonly bytes: ArrayBuffer;
}

export type BrowserLocalReadResult =
  | BrowserLocalReadBytes
  | BrowserSourceReadFailure;

/** File callbacks return data only; the bridge drains engine work separately. */
export interface BrowserLocalFileReader {
  read(
    request: BrowserLocalReadRequest,
  ): Promise<BrowserLocalReadResult>;
}

/** Local immutable source adapter. */
export interface BrowserLocalSource {
  readonly kind: "Local";
  readonly reader: BrowserLocalFileReader;
}

/** Header view used without depending on a particular Fetch implementation. */
export interface BrowserHttpHeaders {
  get(name: string): string | null;
}

/** Strong ETag or an application-provided equivalent immutable validator. */
export type BrowserHttpSnapshotValidator =
  | Readonly<{
    readonly kind: "StrongEtag";
    readonly value: string;
  }>
  | Readonly<{
    readonly kind: "Equivalent";
    readonly header: string;
    readonly value: string;
  }>;

/** Exact bounded request passed to an injected Fetch adapter. */
export interface BrowserHttpRequest {
  readonly url: string;
  readonly method: "GET";
  readonly headers: Readonly<{
    readonly Range: string;
    readonly "If-Range": string;
  }>;
  readonly source: SourceIdentity;
  readonly maximumBytes: number;
  readonly signal: unknown;
}

/** Fully acquired Fetch response, still outside the engine callback stack. */
export interface BrowserHttpResponse {
  readonly type: "Response";
  readonly status: number;
  readonly headers: BrowserHttpHeaders;
  /** Identity observed from this response, never a requested echo. */
  readonly source: SourceIdentity;
  /** Canonical binding of the observed identity and response validator. */
  readonly validator: Uint8Array;
  readonly body: ArrayBuffer;
}

export type BrowserHttpResult =
  | BrowserHttpResponse
  | BrowserSourceReadFailure;

/** Fetch callbacks return one bounded response or one stable failure code. */
export interface BrowserHttpFetcher {
  fetch(request: BrowserHttpRequest): Promise<BrowserHttpResult>;
}

/** HTTP immutable source adapter and its If-Range validator. */
export interface BrowserHttpSource {
  readonly kind: "Http";
  readonly url: string;
  readonly validator: BrowserHttpSnapshotValidator;
  readonly fetcher: BrowserHttpFetcher;
}

export type BrowserSourceAcquirer =
  | BrowserLocalSource
  | BrowserHttpSource;

/** Commands this bridge may submit to an M5-02 adapter. */
export type BrowserSourceCommand =
  | Readonly<{
    readonly type: "ProvideData";
    readonly payload: ProvideDataCommand;
  }>
  | Readonly<{
    readonly type: "FailData";
    readonly payload: FailDataCommand;
  }>;

/** Worker/session binding is explicit so old-epoch output cannot leak. */
export interface BrowserSourceSinkCorrelation {
  readonly worker: bigint;
  readonly session: bigint;
}

/**
 * A sink either synchronously transfers every resource or explicitly adopts
 * exclusive ownership (for example, into the supervisor outbound queue).
 */
export type BrowserSourceSinkOwnership =
  | "Transferred"
  | "AdoptedOwnership";

export interface BrowserSourceSinkReceipt {
  readonly ticket: DataTicket;
  readonly ownership: BrowserSourceSinkOwnership;
}

/** Injected adapter around the current Worker supervisor or transport. */
export interface BrowserSourceCommandSink {
  submit(
    command: BrowserSourceCommand,
    correlation: BrowserSourceSinkCorrelation,
    resources: readonly ArrayBuffer[],
  ): BrowserSourceSinkReceipt;
}

/** Immutable construction data for one source and one live Worker session. */
export interface BrowserSourceBridgeConfiguration {
  readonly worker: bigint;
  readonly session: bigint;
  readonly descriptor: SourceDescriptor;
  readonly source: BrowserSourceAcquirer;
  readonly aborts: BrowserSourceAbortFactory;
  readonly sink: BrowserSourceCommandSink;
  readonly limits: BrowserSourceBridgeLimits;
}

/** Original NeedData owner; request is provenance, not outbound correlation. */
export interface BrowserSourceTicketOwner {
  readonly worker: bigint;
  readonly session: bigint;
  readonly request: bigint;
}

/** One decoded NeedData event and the correlation that carried it. */
export interface BrowserSourceNeedDataRequest {
  readonly need: NeedDataEvent;
  readonly owner: BrowserSourceTicketOwner;
}

/** Stable admission result for a NeedData event. */
export type BrowserSourceRequestResult =
  | "Accepted"
  | "Inactive"
  | "InvalidOwner"
  | "StaleWorker"
  | "ForeignSession"
  | "InvalidNeed"
  | "ForeignSource"
  | "DuplicateTicket"
  | "OverlappingRange"
  | "RangeOutOfBounds"
  | "ActiveTicketLimit"
  | "ResultQueueLimit"
  | "TrackedTicketLimit"
  | "BufferedByteLimit"
  | "AbortFactoryFailure";

/** Observable lifecycle for deterministic host coordination. */
export type BrowserSourceBridgeLifecycle =
  | "Active"
  | "SourceChanged"
  | "Faulted"
  | "Closed";

interface SnapshotLimits {
  readonly maxActiveTickets: number;
  readonly maxQueuedResults: number;
  readonly maxTrackedTickets: number;
  readonly maxBufferedBytes: number;
  readonly maxWholeSourceBytes: number;
  readonly maxDrainTurn: number;
}

interface SnapshotHttpValidator {
  readonly kind: "StrongEtag" | "Equivalent";
  readonly header: string;
  readonly value: string;
}

type SnapshotSource =
  | Readonly<{
    readonly kind: "Local";
    readonly reader: BrowserLocalFileReader;
  }>
  | Readonly<{
    readonly kind: "Http";
    readonly url: string;
    readonly validator: SnapshotHttpValidator;
    readonly fetcher: BrowserHttpFetcher;
  }>;

interface ActiveTicket {
  readonly ticket: DataTicket;
  readonly worker: bigint;
  readonly session: bigint;
  readonly request: bigint;
  readonly token: object;
  readonly ranges: readonly ByteRange[];
  readonly reservedBytes: number;
  readonly controller: BrowserSourceAbortHandle;
  readonly buffers: ArrayBuffer[];
  nextRange: number;
}

interface QueuedSubmission {
  readonly ticket: DataTicket;
  readonly worker: bigint;
  readonly session: bigint;
  readonly request: bigint;
  readonly command: BrowserSourceCommand;
  readonly resources: readonly ArrayBuffer[];
  readonly reservedBytes: number;
}

interface ParsedContentRange {
  readonly start: bigint;
  readonly end: bigint;
  readonly total: bigint;
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

const hasMethods = (
  value: unknown,
  names: readonly string[],
): boolean => {
  if (!isRecord(value)) {
    return false;
  }
  try {
    return names.every(
      (name) => typeof Reflect.get(value, name) === "function",
    );
  } catch {
    return false;
  }
};

const isCapacity = (value: unknown): value is number =>
  typeof value === "number"
  && Number.isSafeInteger(value)
  && value > 0
  && value <= MAX_SOURCE_CAPACITY;

const isByteCapacity = (value: unknown): value is number =>
  typeof value === "number"
  && Number.isSafeInteger(value)
  && value > 0
  && value <= MAX_SOURCE_BYTES;

const snapshotLimits = (
  value: BrowserSourceBridgeLimits,
): SnapshotLimits | undefined => {
  try {
    if (
      !isCapacity(value.maxActiveTickets)
      || !isCapacity(value.maxQueuedResults)
      || !isCapacity(value.maxTrackedTickets)
      || !isByteCapacity(value.maxBufferedBytes)
      || !isByteCapacity(value.maxWholeSourceBytes)
      || !isCapacity(value.maxDrainTurn)
      || value.maxQueuedResults < value.maxActiveTickets
      || value.maxTrackedTickets < value.maxActiveTickets
    ) {
      return undefined;
    }
    return Object.freeze({
      maxActiveTickets: value.maxActiveTickets,
      maxQueuedResults: value.maxQueuedResults,
      maxTrackedTickets: value.maxTrackedTickets,
      maxBufferedBytes: value.maxBufferedBytes,
      maxWholeSourceBytes: value.maxWholeSourceBytes,
      maxDrainTurn: value.maxDrainTurn,
    });
  } catch {
    return undefined;
  }
};

const isHeaderName = (value: unknown): value is string =>
  typeof value === "string"
  && value.length > 0
  && value.length <= MAX_BROWSER_HTTP_VALIDATOR_HEADER_NAME_LENGTH
  && /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/u.test(value);

const isHeaderValue = (value: unknown): value is string =>
  typeof value === "string"
  && value.length > 0
  && value.length <= MAX_BROWSER_HTTP_VALIDATOR_VALUE_LENGTH
  && !/[\u0000-\u001f\u007f]/u.test(value);

const isHttpUrl = (value: string): boolean => {
  try {
    const parsed = new URL(value);
    return parsed.protocol === "http:"
      || parsed.protocol === "https:";
  } catch {
    return false;
  }
};

const isStrongEtag = (value: unknown): value is string => {
  if (
    typeof value !== "string"
    || value.length < 2
    || value.length > MAX_BROWSER_HTTP_VALIDATOR_VALUE_LENGTH
    || value.startsWith("W/")
    || value[0] !== "\""
    || value[value.length - 1] !== "\""
  ) {
    return false;
  }
  const opaque = value.slice(1, -1);
  return !/[\u0000-\u0020"\u007f]/u.test(opaque);
};

const snapshotValidator = (
  value: BrowserHttpSnapshotValidator,
): SnapshotHttpValidator | undefined => {
  try {
    const kind = value.kind;
    if (kind === "StrongEtag") {
      const validatorValue = value.value;
      return isStrongEtag(validatorValue)
        ? Object.freeze({
          kind: "StrongEtag",
          header: "etag",
          value: validatorValue,
        })
        : undefined;
    }
    const header = value.header;
    const validatorValue = value.value;
    if (
      kind !== "Equivalent"
      || !isHeaderName(header)
      || !isHeaderValue(validatorValue)
    ) {
      return undefined;
    }
    return Object.freeze({
      kind: "Equivalent",
      header: header.toLowerCase(),
      value: validatorValue,
    });
  } catch {
    return undefined;
  }
};

const SHA256_ROUND_CONSTANTS = Object.freeze([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
  0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
  0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
  0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
  0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
  0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
  0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
  0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
  0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

const rotateRight = (value: number, count: number): number =>
  (value >>> count) | (value << (32 - count));

const sha256 = (input: Uint8Array): Uint8Array => {
  const bitLength = input.byteLength * 8;
  const paddedLength =
    Math.ceil((input.byteLength + 9) / 64) * 64;
  const padded = new Uint8Array(paddedLength);
  padded.set(input);
  padded[input.byteLength] = 0x80;
  const paddedView = new DataView(padded.buffer);
  paddedView.setUint32(
    paddedLength - 8,
    Math.floor(bitLength / 0x1_0000_0000),
    false,
  );
  paddedView.setUint32(
    paddedLength - 4,
    bitLength >>> 0,
    false,
  );

  const state = new Uint32Array([
    0x6a09e667,
    0xbb67ae85,
    0x3c6ef372,
    0xa54ff53a,
    0x510e527f,
    0x9b05688c,
    0x1f83d9ab,
    0x5be0cd19,
  ]);
  const words = new Uint32Array(64);
  for (let offset = 0; offset < paddedLength; offset += 64) {
    for (let index = 0; index < 16; index += 1) {
      words[index] = paddedView.getUint32(
        offset + index * 4,
        false,
      );
    }
    for (let index = 16; index < 64; index += 1) {
      const before15 = words[index - 15]!;
      const before2 = words[index - 2]!;
      const sigma0 =
        rotateRight(before15, 7)
        ^ rotateRight(before15, 18)
        ^ (before15 >>> 3);
      const sigma1 =
        rotateRight(before2, 17)
        ^ rotateRight(before2, 19)
        ^ (before2 >>> 10);
      words[index] = (
        words[index - 16]!
        + sigma0
        + words[index - 7]!
        + sigma1
      ) >>> 0;
    }

    let a = state[0]!;
    let b = state[1]!;
    let c = state[2]!;
    let d = state[3]!;
    let e = state[4]!;
    let f = state[5]!;
    let g = state[6]!;
    let h = state[7]!;
    for (let index = 0; index < 64; index += 1) {
      const sum1 =
        rotateRight(e, 6)
        ^ rotateRight(e, 11)
        ^ rotateRight(e, 25);
      const choose = (e & f) ^ (~e & g);
      const temporary1 = (
        h
        + sum1
        + choose
        + SHA256_ROUND_CONSTANTS[index]!
        + words[index]!
      ) >>> 0;
      const sum0 =
        rotateRight(a, 2)
        ^ rotateRight(a, 13)
        ^ rotateRight(a, 22);
      const majority = (a & b) ^ (a & c) ^ (b & c);
      const temporary2 = (sum0 + majority) >>> 0;
      h = g;
      g = f;
      f = e;
      e = (d + temporary1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (temporary1 + temporary2) >>> 0;
    }
    state[0] = (state[0]! + a) >>> 0;
    state[1] = (state[1]! + b) >>> 0;
    state[2] = (state[2]! + c) >>> 0;
    state[3] = (state[3]! + d) >>> 0;
    state[4] = (state[4]! + e) >>> 0;
    state[5] = (state[5]! + f) >>> 0;
    state[6] = (state[6]! + g) >>> 0;
    state[7] = (state[7]! + h) >>> 0;
  }
  const output = new Uint8Array(32);
  const outputView = new DataView(output.buffer);
  state.forEach((word, index) => {
    outputView.setUint32(index * 4, word, false);
  });
  return output;
};

const concatenateBytes = (
  values: readonly Uint8Array[],
): Uint8Array => {
  const length = values.reduce(
    (total, value) => total + value.byteLength,
    0,
  );
  const result = new Uint8Array(length);
  let offset = 0;
  for (const value of values) {
    result.set(value, offset);
    offset += value.byteLength;
  }
  return result;
};

const encodeU32 = (value: number): Uint8Array => {
  const encoded = new Uint8Array(4);
  new DataView(encoded.buffer).setUint32(0, value, false);
  return encoded;
};

const encodeU64 = (value: bigint): Uint8Array => {
  const encoded = new Uint8Array(8);
  new DataView(encoded.buffer).setBigUint64(0, value, false);
  return encoded;
};

const validatorBinding = (
  identity: SourceIdentity,
  validator: SnapshotHttpValidator,
): Uint8Array => {
  const encoder = new TextEncoder();
  const domain = encoder.encode(
    "pdf-rs.browser-source-validator.v1",
  );
  const header = encoder.encode(validator.header);
  const value = encoder.encode(validator.value);
  return sha256(concatenateBytes([
    domain,
    Uint8Array.of(validator.kind === "StrongEtag" ? 1 : 2),
    encodeU32(header.byteLength),
    header,
    encodeU32(value.byteLength),
    value,
    identity.stable_id,
    encodeU64(identity.revision),
  ]));
};

const fixedBytesEqual = (
  left: Uint8Array,
  right: Uint8Array,
): boolean => {
  if (left.byteLength !== right.byteLength) {
    return false;
  }
  let difference = 0;
  for (let index = 0; index < left.byteLength; index += 1) {
    difference |= left[index]! ^ right[index]!;
  }
  return difference === 0;
};

/**
 * Canonical SHA-256 binding stored in `SourceDescriptor.validator`.
 *
 * The binding includes validator kind/header/value and the complete
 * SourceIdentity, preventing a textual validator from being relabelled onto a
 * different source or revision.
 */
export const deriveBrowserHttpValidatorBinding = (
  identity: SourceIdentity,
  validator: BrowserHttpSnapshotValidator,
): Uint8Array => {
  const snapshot = snapshotValidator(validator);
  if (
    snapshot === undefined
    || !validateSourceIdentity(identity)
  ) {
    throw new BrowserSourceBridgeError("InvalidConfiguration");
  }
  try {
    return validatorBinding(
      snapshotSourceIdentity(identity),
      snapshot,
    );
  } catch {
    throw new BrowserSourceBridgeError("InvalidConfiguration");
  }
};

const snapshotSource = (
  value: BrowserSourceAcquirer,
  descriptor: SourceDescriptor,
): SnapshotSource | undefined => {
  try {
    if (value.kind === "Local") {
      if (
        descriptor.length === undefined
        || descriptor.length === 0n
        || !hasMethods(value.reader, ["read"])
      ) {
        return undefined;
      }
      return Object.freeze({
        kind: "Local",
        reader: value.reader,
      });
    }
    if (
      value.kind !== "Http"
      || typeof value.url !== "string"
      || value.url.length === 0
      || value.url.length > MAX_URL_LENGTH
      || !isHttpUrl(value.url)
      || /[\u0000-\u001f\u007f]/u.test(value.url)
      || !hasMethods(value.fetcher, ["fetch"])
    ) {
      return undefined;
    }
    const validator = snapshotValidator(value.validator);
    if (
      validator === undefined
      || !fixedBytesEqual(
        validatorBinding(descriptor.identity, validator),
        descriptor.validator,
      )
    ) {
      return undefined;
    }
    return Object.freeze({
      kind: "Http",
      url: value.url,
      validator,
      fetcher: value.fetcher,
    });
  } catch {
    return undefined;
  }
};

const isFixedArrayBuffer = (value: unknown): value is ArrayBuffer => {
  try {
    return value instanceof ArrayBuffer
      && Object.getPrototypeOf(value) === ArrayBuffer.prototype
      && (
        Object.getOwnPropertyDescriptor(
          ArrayBuffer.prototype,
          "resizable",
        ) === undefined
        || value.resizable === false
      );
  } catch {
    return false;
  }
};

const takeBufferOwnership = (
  value: unknown,
): ArrayBuffer | undefined => {
  if (!isFixedArrayBuffer(value)) {
    return undefined;
  }
  try {
    const owned = structuredClone(value, { transfer: [value] });
    return isFixedArrayBuffer(owned) && value.byteLength === 0
      ? owned
      : undefined;
  } catch {
    return undefined;
  }
};

const discardBuffer = (value: unknown): void => {
  try {
    if (
      value instanceof ArrayBuffer
      && value.byteLength !== 0
    ) {
      structuredClone(value, { transfer: [value] });
    }
  } catch {
    // Dropping the only retained reference remains the terminal fallback.
  }
};

const discardBuffers = (values: readonly unknown[]): void => {
  for (const value of values) {
    discardBuffer(value);
  }
};

const discardCompletionBytes = (value: unknown): void => {
  if (!isRecord(value)) {
    return;
  }
  try {
    if (value.type === "Response") {
      discardBuffer(value.body);
    } else if (value.type === "Bytes") {
      discardBuffer(value.bytes);
    }
  } catch {
    // No callback data is retained.
  }
};

const sourceIdentityEqual = (
  left: SourceIdentity,
  right: SourceIdentity,
): boolean => {
  try {
    return left.revision === right.revision
      && left.stable_id.byteLength === right.stable_id.byteLength
      && left.stable_id.every(
        (byte, index) => byte === right.stable_id[index],
      );
  } catch {
    return false;
  }
};

const snapshotObservedValidator = (
  expected: SnapshotHttpValidator,
  value: unknown,
): SnapshotHttpValidator | undefined => {
  if (typeof value !== "string") {
    return undefined;
  }
  return expected.kind === "StrongEtag"
    ? snapshotValidator({
      kind: "StrongEtag",
      value,
    })
    : snapshotValidator({
      kind: "Equivalent",
      header: expected.header,
      value,
    });
};

const syntheticObservedIdentity = (
  expected: SourceIdentity,
  validatorValue: unknown,
  validatorBytes: unknown,
  reportedIdentity: unknown,
): SourceIdentity => {
  const boundedValue =
    typeof validatorValue === "string"
    && validatorValue.length <= MAX_BROWSER_HTTP_VALIDATOR_VALUE_LENGTH
      ? validatorValue
      : "<missing-or-invalid-validator>";
  const bindingEvidence =
    validatorBytes instanceof Uint8Array
    && validatorBytes.byteLength <= 64
      ? validatorBytes
      : new Uint8Array(0);
  const identityEvidence = validateSourceIdentity(reportedIdentity)
    ? concatenateBytes([
      reportedIdentity.stable_id,
      encodeU64(reportedIdentity.revision),
    ])
    : new Uint8Array(0);
  const encoder = new TextEncoder();
  const stableId = sha256(concatenateBytes([
    encoder.encode("pdf-rs.browser-observed-source.v1"),
    expected.stable_id,
    encodeU64(expected.revision),
    encoder.encode(boundedValue),
    bindingEvidence,
    identityEvidence,
  ]));
  if (
    !stableId.some((byte) => byte !== 0)
    || fixedBytesEqual(stableId, expected.stable_id)
  ) {
    stableId[0] = (stableId[0]! ^ 0x80) || 1;
  }
  return Object.freeze({
    stable_id: stableId,
    revision: expected.revision,
  });
};

const snapshotRanges = (
  ranges: readonly ByteRange[],
): readonly ByteRange[] =>
  Object.freeze(
    ranges.map((range) =>
      Object.freeze({
        start: range.start,
        len: range.len,
      })),
  );

const rangeEnd = (range: ByteRange): bigint =>
  range.start + range.len;

const rangesOverlap = (
  left: ByteRange,
  right: ByteRange,
): boolean =>
  left.start < rangeEnd(right)
  && right.start < rangeEnd(left);

const requestedBytes = (
  ranges: readonly ByteRange[],
): number => {
  let total = 0n;
  for (const range of ranges) {
    total += range.len;
  }
  return Number(total);
};

const rangesWithin = (
  ranges: readonly ByteRange[],
  length: bigint,
): boolean =>
  ranges.every((range) => rangeEnd(range) <= length);

const parseU64Header = (value: string | null): bigint | undefined => {
  if (
    value === null
    || !/^(0|[1-9][0-9]*)$/u.test(value)
  ) {
    return undefined;
  }
  try {
    const parsed = BigInt(value);
    return parsed <= MAX_U64 ? parsed : undefined;
  } catch {
    return undefined;
  }
};

const parseContentRange = (
  value: string | null,
): ParsedContentRange | undefined => {
  if (value === null) {
    return undefined;
  }
  const match = /^bytes (0|[1-9][0-9]*)-(0|[1-9][0-9]*)\/(0|[1-9][0-9]*)$/u
    .exec(value);
  if (match === null) {
    return undefined;
  }
  try {
    const start = BigInt(match[1]!);
    const end = BigInt(match[2]!);
    const total = BigInt(match[3]!);
    if (
      start > MAX_U64
      || end > MAX_U64
      || total > MAX_U64
      || total === 0n
      || end < start
      || end >= total
    ) {
      return undefined;
    }
    return Object.freeze({ start, end, total });
  } catch {
    return undefined;
  }
};

const retryableFailure = (code: SourceFailureCode): boolean => {
  switch (code) {
    case SourceFailureCode.SourceChanged:
    case SourceFailureCode.PermissionDenied:
    case SourceFailureCode.InvalidRangeResponse:
      return false;
    case SourceFailureCode.Unavailable:
    case SourceFailureCode.Timeout:
    case SourceFailureCode.Truncated:
    case SourceFailureCode.TransportFailure:
      return true;
  }
};

const failureForHttpStatus = (
  status: number,
): SourceFailureCode => {
  if (status === 401 || status === 403) {
    return SourceFailureCode.PermissionDenied;
  }
  if (status === 408 || status === 504) {
    return SourceFailureCode.Timeout;
  }
  if (
    status === 404
    || status === 410
    || status === 429
    || status === 500
    || status === 502
    || status === 503
  ) {
    return SourceFailureCode.Unavailable;
  }
  return SourceFailureCode.InvalidRangeResponse;
};

const normalizedFailure = (
  value: unknown,
  expected: SourceIdentity,
): Readonly<{
  readonly code: SourceFailureCode;
  readonly observed?: SourceIdentity;
}> | undefined => {
  if (!isRecord(value)) {
    return undefined;
  }
  try {
    if (
      value.type !== "Failure"
      || !validateSourceFailureCode(value.code)
    ) {
      return undefined;
    }
    if (value.code === SourceFailureCode.SourceChanged) {
      if (
        !validateSourceIdentity(value.observed)
        || sourceIdentityEqual(value.observed, expected)
      ) {
        return undefined;
      }
      return Object.freeze({
        code: value.code,
        observed: snapshotSourceIdentity(value.observed),
      });
    }
    if (value.observed !== undefined) {
      return undefined;
    }
    return Object.freeze({ code: value.code });
  } catch {
    return undefined;
  }
};

const validAbortHandle = (
  value: unknown,
): value is BrowserSourceAbortHandle =>
  isRecord(value) && hasMethods(value, ["abort"]);

const validSinkReceipt = (
  value: unknown,
  ticket: DataTicket,
  resources: readonly ArrayBuffer[],
): boolean => {
  if (!isRecord(value)) {
    return false;
  }
  try {
    return value.ticket === ticket
      && (
        value.ownership === "Transferred"
        || value.ownership === "AdoptedOwnership"
      )
      && (
        value.ownership !== "Transferred"
        || resources.every(
          (resource) => resource.byteLength === 0,
        )
      );
  } catch {
    return false;
  }
};

/**
 * Bounded immutable source bridge.
 *
 * Fetch and file completions validate and queue protocol commands only.
 * `drain` is the sole path that invokes the injected Worker/supervisor sink.
 */
export class BrowserSourceBridge {
  readonly #descriptor: SourceDescriptor;
  readonly #source: SnapshotSource;
  readonly #aborts: BrowserSourceAbortFactory;
  readonly #sink: BrowserSourceCommandSink;
  readonly #limits: SnapshotLimits;
  #worker: bigint;
  #session: bigint;
  #lifecycle: BrowserSourceBridgeLifecycle = "Active";
  #epochToken: object = {};
  #knownLength: bigint | undefined;
  #wholeSource: ArrayBuffer | undefined;
  #reservedBytes = 0;
  readonly #active = new Map<DataTicket, ActiveTicket>();
  readonly #queued: QueuedSubmission[] = [];
  readonly #trackedTickets = new Set<DataTicket>();
  readonly #reservedRanges =
    new Map<DataTicket, readonly ByteRange[]>();

  constructor(configuration: BrowserSourceBridgeConfiguration) {
    let descriptor: SourceDescriptor;
    let limits: SnapshotLimits | undefined;
    let source: SnapshotSource | undefined;
    try {
      if (
        !validateWorkerId(configuration.worker)
        || configuration.worker === 0n
        || !validateSessionId(configuration.session)
        || configuration.session === 0n
        || !validateSourceDescriptor(configuration.descriptor)
        || !hasMethods(configuration.aborts, ["create"])
        || !hasMethods(configuration.sink, ["submit"])
      ) {
        throw new BrowserSourceBridgeError("InvalidConfiguration");
      }
      descriptor = snapshotSourceDescriptor(configuration.descriptor);
      limits = snapshotLimits(configuration.limits);
      source = snapshotSource(configuration.source, descriptor);
    } catch (error) {
      if (error instanceof BrowserSourceBridgeError) {
        throw error;
      }
      throw new BrowserSourceBridgeError("InvalidConfiguration");
    }
    if (limits === undefined || source === undefined) {
      throw new BrowserSourceBridgeError("InvalidConfiguration");
    }
    this.#worker = configuration.worker;
    this.#session = configuration.session;
    this.#descriptor = descriptor;
    this.#knownLength = descriptor.length;
    this.#source = source;
    this.#aborts = configuration.aborts;
    this.#sink = configuration.sink;
    this.#limits = limits;
  }

  get lifecycle(): BrowserSourceBridgeLifecycle {
    return this.#lifecycle;
  }

  get worker(): bigint {
    return this.#worker;
  }

  get session(): bigint {
    return this.#session;
  }

  get descriptor(): SourceDescriptor {
    return snapshotSourceDescriptor(this.#descriptor);
  }

  get knownLength(): bigint | undefined {
    return this.#knownLength;
  }

  get activeTickets(): number {
    return this.#active.size;
  }

  get queuedResults(): number {
    return this.#queued.length;
  }

  get trackedTickets(): number {
    return this.#trackedTickets.size;
  }

  get bufferedBytes(): number {
    return this.#reservedBytes;
  }

  get hasWholeSourceSnapshot(): boolean {
    return this.#wholeSource !== undefined;
  }

  /** Validates and starts one exact NeedData acquisition. */
  request(
    input: BrowserSourceNeedDataRequest,
  ): BrowserSourceRequestResult {
    if (this.#lifecycle !== "Active") {
      return "Inactive";
    }
    let need: NeedDataEvent;
    let owner: BrowserSourceTicketOwner;
    try {
      need = input.need;
      owner = input.owner;
    } catch {
      return "InvalidOwner";
    }
    let ownerWorker: bigint;
    let ownerSession: bigint;
    let ownerRequest: bigint;
    try {
      if (!isRecord(owner)) {
        return "InvalidOwner";
      }
      ownerWorker = owner.worker;
      ownerSession = owner.session;
      ownerRequest = owner.request;
    } catch {
      return "InvalidOwner";
    }
    if (
      !validateWorkerId(ownerWorker)
      || ownerWorker === 0n
      || !validateSessionId(ownerSession)
      || ownerSession === 0n
      || !validateRequestId(ownerRequest)
      || ownerRequest === 0n
    ) {
      return "InvalidOwner";
    }
    if (ownerWorker !== this.#worker) {
      return "StaleWorker";
    }
    if (ownerSession !== this.#session) {
      return "ForeignSession";
    }
    if (
      !validateNeedDataEvent(need)
      || need.checkpoint === 0n
    ) {
      return "InvalidNeed";
    }
    if (!sourceIdentityEqual(need.source, this.#descriptor.identity)) {
      return "ForeignSource";
    }
    if (this.#trackedTickets.has(need.ticket)) {
      return "DuplicateTicket";
    }
    const ranges = snapshotRanges(need.ranges);
    if (
      this.#knownLength !== undefined
      && !rangesWithin(ranges, this.#knownLength)
    ) {
      return "RangeOutOfBounds";
    }
    for (const reserved of this.#reservedRanges.values()) {
      if (
        ranges.some((range) =>
          reserved.some((other) => rangesOverlap(range, other)),
        )
      ) {
        return "OverlappingRange";
      }
    }
    if (this.#active.size >= this.#limits.maxActiveTickets) {
      return "ActiveTicketLimit";
    }
    if (
      this.#active.size + this.#queued.length
      >= this.#limits.maxQueuedResults
    ) {
      return "ResultQueueLimit";
    }
    if (
      this.#trackedTickets.size
      >= this.#limits.maxTrackedTickets
    ) {
      return "TrackedTicketLimit";
    }
    const reservedBytes = requestedBytes(ranges);
    if (
      reservedBytes > this.#limits.maxBufferedBytes
      || this.#reservedBytes
        > this.#limits.maxBufferedBytes - reservedBytes
    ) {
      return "BufferedByteLimit";
    }

    let controller: BrowserSourceAbortHandle;
    try {
      controller = this.#aborts.create();
    } catch {
      return "AbortFactoryFailure";
    }
    if (!validAbortHandle(controller)) {
      return "AbortFactoryFailure";
    }

    const active: ActiveTicket = {
      ticket: need.ticket,
      worker: ownerWorker,
      session: ownerSession,
      request: ownerRequest,
      token: this.#epochToken,
      ranges,
      reservedBytes,
      controller,
      buffers: [],
      nextRange: 0,
    };
    this.#trackedTickets.add(need.ticket);
    this.#reservedRanges.set(need.ticket, ranges);
    this.#reservedBytes += reservedBytes;
    this.#active.set(need.ticket, active);

    if (this.#wholeSource !== undefined) {
      this.#serveWholeSource(active);
    } else if (this.#source.kind === "Local") {
      this.#startLocalRead(active);
    } else {
      this.#startHttpRead(active);
    }
    return "Accepted";
  }

  /** Converts a slow active ticket into a stable Timeout result. */
  timeout(
    ticket: DataTicket,
    owner: BrowserSourceTicketOwner,
  ): boolean {
    const active = this.#active.get(ticket);
    if (
      active === undefined
      || !this.#activeOwnerMatches(active, owner)
    ) {
      return false;
    }
    this.#queueFailure(active, SourceFailureCode.Timeout);
    this.#abort(active.controller);
    return true;
  }

  /** Aborts active work or drops an undrained result for one ticket. */
  cancel(
    ticket: DataTicket,
    owner: BrowserSourceTicketOwner,
  ): boolean {
    const active = this.#active.get(ticket);
    if (
      active !== undefined
      && this.#activeOwnerMatches(active, owner)
    ) {
      this.#dropActive(active);
      return true;
    }
    const index = this.#queued.findIndex(
      (queued) =>
        queued.ticket === ticket
        && this.#queuedOwnerMatches(queued, owner),
    );
    if (index < 0) {
      return false;
    }
    const [queued] = this.#queued.splice(index, 1);
    if (queued === undefined) {
      return false;
    }
    this.#releaseQueued(queued);
    return true;
  }

  /**
   * Submits validated commands outside acquisition callback stacks.
   *
   * ProvideData resources are exact fixed ArrayBuffers. The sink receipt must
   * either prove synchronous detachment or explicitly accept ownership.
   */
  drain(maximum = this.#limits.maxDrainTurn): number {
    if (
      !Number.isSafeInteger(maximum)
      || maximum <= 0
      || maximum > this.#limits.maxDrainTurn
    ) {
      throw new BrowserSourceBridgeError("InvalidTurnBudget");
    }
    if (
      this.#lifecycle !== "Active"
      && this.#lifecycle !== "SourceChanged"
    ) {
      return 0;
    }
    let submitted = 0;
    while (
      submitted < maximum
      && (
        this.#lifecycle === "Active"
        || this.#lifecycle === "SourceChanged"
      )
    ) {
      const queued = this.#queued.shift();
      if (queued === undefined) {
        break;
      }
      this.#releaseReservation(queued);
      if (
        queued.worker !== this.#worker
        || queued.session !== this.#session
        || !validateRequestId(queued.request)
        || queued.request === 0n
        || !this.#validateQueued(queued)
      ) {
        discardBuffers(queued.resources);
        this.#enterFaulted();
        throw new BrowserSourceBridgeError("SinkOwnershipFailure");
      }
      const resources = Object.freeze([...queued.resources]);
      let receipt: unknown;
      try {
        receipt = this.#sink.submit(
          queued.command,
          Object.freeze({
            worker: queued.worker,
            session: queued.session,
          }),
          resources,
        );
      } catch {
        discardBuffers(resources);
        this.#enterFaulted();
        throw new BrowserSourceBridgeError("SinkFailure");
      }
      if (!validSinkReceipt(receipt, queued.ticket, resources)) {
        discardBuffers(resources);
        this.#enterFaulted();
        throw new BrowserSourceBridgeError("SinkOwnershipFailure");
      }
      submitted += 1;
    }
    return submitted;
  }

  /** Invalidates all work for the current Worker epoch. */
  fault(): void {
    if (this.#lifecycle === "Closed") {
      return;
    }
    this.#enterFaulted();
  }

  /** Starts a strictly newer Worker epoch with no old ticket ownership. */
  restart(worker: bigint, session: bigint): void {
    if (
      this.#lifecycle === "Closed"
      || this.#lifecycle === "SourceChanged"
      || !validateWorkerId(worker)
      || worker === 0n
      || worker <= this.#worker
      || !validateSessionId(session)
      || session === 0n
    ) {
      throw new BrowserSourceBridgeError("InvalidLifecycle");
    }
    this.#invalidatePending();
    this.#worker = worker;
    this.#session = session;
    this.#lifecycle = "Active";
    this.#trackedTickets.clear();
  }

  /** Terminally closes the source and releases every retained byte. */
  close(): void {
    if (this.#lifecycle === "Closed") {
      return;
    }
    this.#invalidatePending();
    this.#lifecycle = "Closed";
    this.#trackedTickets.clear();
  }

  #isActive(active: ActiveTicket): boolean {
    return this.#lifecycle === "Active"
      && active.worker === this.#worker
      && active.session === this.#session
      && active.request !== 0n
      && active.token === this.#epochToken
      && this.#active.get(active.ticket) === active;
  }

  #activeOwnerMatches(
    active: ActiveTicket,
    owner: BrowserSourceTicketOwner,
  ): boolean {
    try {
      return active.worker === owner.worker
        && active.session === owner.session
        && active.request === owner.request;
    } catch {
      return false;
    }
  }

  #queuedOwnerMatches(
    queued: QueuedSubmission,
    owner: BrowserSourceTicketOwner,
  ): boolean {
    try {
      return queued.worker === owner.worker
        && queued.session === owner.session
        && queued.request === owner.request;
    } catch {
      return false;
    }
  }

  #startLocalRead(active: ActiveTicket): void {
    if (!this.#isActive(active)) {
      return;
    }
    const range = active.ranges[active.nextRange];
    if (range === undefined || this.#source.kind !== "Local") {
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    let request: BrowserLocalReadRequest;
    try {
      request = Object.freeze({
        source: snapshotSourceIdentity(this.#descriptor.identity),
        range: Object.freeze({
          start: range.start,
          len: range.len,
        }),
        maximumBytes: Number(range.len),
        signal: active.controller.signal,
      });
    } catch {
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    let completion: Promise<BrowserLocalReadResult>;
    try {
      completion = this.#source.reader.read(request);
    } catch {
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    Promise.resolve(completion).then(
      (result): void => {
        this.#acceptLocalResult(active, range, result);
      },
      (): void => {
        if (this.#isActive(active)) {
          this.#queueFailure(
            active,
            SourceFailureCode.TransportFailure,
          );
        }
      },
    );
  }

  #acceptLocalResult(
    active: ActiveTicket,
    requested: ByteRange,
    result: BrowserLocalReadResult,
  ): void {
    if (!this.#isActive(active)) {
      discardCompletionBytes(result);
      return;
    }
    const failure = normalizedFailure(
      result,
      this.#descriptor.identity,
    );
    if (failure !== undefined) {
      this.#queueFailure(
        active,
        failure.code,
        failure.observed,
      );
      return;
    }
    if (!isRecord(result) || result.type !== "Bytes") {
      discardCompletionBytes(result);
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    let observed: SourceIdentity;
    let returnedRange: ByteRange;
    let totalLength: bigint;
    let bytes: unknown;
    try {
      observed = result.source;
      returnedRange = result.range;
      totalLength = result.totalLength;
      bytes = result.bytes;
    } catch {
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    if (!validateSourceIdentity(observed)) {
      discardBuffer(bytes);
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    if (!sourceIdentityEqual(observed, this.#descriptor.identity)) {
      discardBuffer(bytes);
      this.#queueFailure(
        active,
        SourceFailureCode.SourceChanged,
        observed,
      );
      return;
    }
    if (
      !isRecord(returnedRange)
      || returnedRange.start !== requested.start
      || returnedRange.len !== requested.len
      || typeof totalLength !== "bigint"
      || totalLength === 0n
      || totalLength > MAX_U64
      || !this.#totalLengthCompatible(totalLength)
      || rangeEnd(requested) > totalLength
    ) {
      discardBuffer(bytes);
      this.#queueFailure(
        active,
        SourceFailureCode.InvalidRangeResponse,
      );
      return;
    }
    const ownedBytes = takeBufferOwnership(bytes);
    if (ownedBytes === undefined) {
      discardBuffer(bytes);
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    if (BigInt(ownedBytes.byteLength) !== requested.len) {
      const code = BigInt(ownedBytes.byteLength) < requested.len
        ? SourceFailureCode.Truncated
        : SourceFailureCode.InvalidRangeResponse;
      discardBuffer(ownedBytes);
      this.#queueFailure(active, code);
      return;
    }
    this.#commitTotalLength(totalLength);
    this.#acceptRangeBytes(active, ownedBytes);
  }

  #startHttpRead(active: ActiveTicket): void {
    if (!this.#isActive(active)) {
      return;
    }
    const range = active.ranges[active.nextRange];
    if (range === undefined || this.#source.kind !== "Http") {
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    const inclusiveEnd = rangeEnd(range) - 1n;
    let request: BrowserHttpRequest;
    try {
      request = Object.freeze({
        url: this.#source.url,
        method: "GET",
        headers: Object.freeze({
          Range: `bytes=${range.start}-${inclusiveEnd}`,
          "If-Range": this.#source.validator.value,
        }),
        source: snapshotSourceIdentity(this.#descriptor.identity),
        maximumBytes: Math.max(
          Number(range.len),
          this.#limits.maxWholeSourceBytes,
        ),
        signal: active.controller.signal,
      });
    } catch {
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    let completion: Promise<BrowserHttpResult>;
    try {
      completion = this.#source.fetcher.fetch(request);
    } catch {
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    Promise.resolve(completion).then(
      (result): void => {
        this.#acceptHttpResult(active, range, result);
      },
      (): void => {
        if (this.#isActive(active)) {
          this.#queueFailure(
            active,
            SourceFailureCode.TransportFailure,
          );
        }
      },
    );
  }

  #acceptHttpResult(
    active: ActiveTicket,
    requested: ByteRange,
    result: BrowserHttpResult,
  ): void {
    if (!this.#isActive(active)) {
      discardCompletionBytes(result);
      return;
    }
    const failure = normalizedFailure(
      result,
      this.#descriptor.identity,
    );
    if (failure !== undefined) {
      this.#queueFailure(
        active,
        failure.code,
        failure.observed,
      );
      return;
    }
    if (!isRecord(result) || result.type !== "Response") {
      discardCompletionBytes(result);
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    let status: number;
    let headers: BrowserHttpHeaders;
    let body: unknown;
    try {
      status = result.status;
      headers = result.headers;
      body = result.body;
    } catch {
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    if (
      !Number.isSafeInteger(status)
      || !hasMethods(headers, ["get"])
      || !isFixedArrayBuffer(body)
    ) {
      discardBuffer(body);
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    const ownedBody = takeBufferOwnership(body);
    if (ownedBody === undefined) {
      discardBuffer(body);
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    if (this.#source.kind !== "Http") {
      discardBuffer(ownedBody);
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    if (status !== 200 && status !== 206) {
      discardBuffer(ownedBody);
      this.#queueFailure(active, failureForHttpStatus(status));
      return;
    }
    let observed: unknown;
    let observedBinding: unknown;
    try {
      observed = result.source;
      observedBinding = result.validator;
    } catch {
      observed = undefined;
      observedBinding = undefined;
    }
    let responseValidator: unknown;
    try {
      responseValidator = headers.get(
        this.#source.validator.header,
      );
    } catch {
      discardBuffer(ownedBody);
      this.#queueFailure(
        active,
        SourceFailureCode.SourceChanged,
        syntheticObservedIdentity(
          this.#descriptor.identity,
          undefined,
          observedBinding,
          observed,
        ),
      );
      return;
    }
    const observedValidator = snapshotObservedValidator(
      this.#source.validator,
      responseValidator,
    );
    let trustedObserved = false;
    if (
      validateSourceIdentity(observed)
      && observedValidator !== undefined
      && observedBinding instanceof Uint8Array
      && observedBinding.byteLength === 32
    ) {
      try {
        trustedObserved = fixedBytesEqual(
          validatorBinding(observed, observedValidator),
          observedBinding,
        );
      } catch {
        trustedObserved = false;
      }
    }
    const expectedSnapshot = trustedObserved
      && validateSourceIdentity(observed)
      && sourceIdentityEqual(
        observed,
        this.#descriptor.identity,
      )
      && responseValidator === this.#source.validator.value
      && observedBinding instanceof Uint8Array
      && fixedBytesEqual(
        observedBinding,
        this.#descriptor.validator,
      );
    if (!expectedSnapshot) {
      discardBuffer(ownedBody);
      const observedIdentity =
        trustedObserved
        && validateSourceIdentity(observed)
        && !sourceIdentityEqual(
          observed,
          this.#descriptor.identity,
        )
          ? snapshotSourceIdentity(observed)
          : syntheticObservedIdentity(
            this.#descriptor.identity,
            responseValidator,
            observedBinding,
            observed,
          );
      this.#queueFailure(
        active,
        SourceFailureCode.SourceChanged,
        observedIdentity,
      );
      return;
    }
    if (status === 206) {
      this.#acceptPartialHttpResponse(
        active,
        requested,
        headers,
        ownedBody,
      );
      return;
    }
    if (status === 200) {
      this.#acceptWholeHttpResponse(active, headers, ownedBody);
      return;
    }
  }

  #acceptPartialHttpResponse(
    active: ActiveTicket,
    requested: ByteRange,
    headers: BrowserHttpHeaders,
    body: ArrayBuffer,
  ): void {
    let contentRange: ParsedContentRange | undefined;
    let contentLength: bigint | undefined;
    try {
      contentRange = parseContentRange(
        headers.get("content-range"),
      );
      contentLength = parseU64Header(
        headers.get("content-length"),
      );
    } catch {
      discardBuffer(body);
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    const requestedEnd = rangeEnd(requested) - 1n;
    if (
      contentRange === undefined
      || contentRange.start !== requested.start
      || contentRange.end !== requestedEnd
      || contentLength !== requested.len
      || !this.#totalLengthCompatible(contentRange.total)
      || rangeEnd(requested) > contentRange.total
    ) {
      discardBuffer(body);
      this.#queueFailure(
        active,
        SourceFailureCode.InvalidRangeResponse,
      );
      return;
    }
    if (BigInt(body.byteLength) !== requested.len) {
      const code = BigInt(body.byteLength) < requested.len
        ? SourceFailureCode.Truncated
        : SourceFailureCode.InvalidRangeResponse;
      discardBuffer(body);
      this.#queueFailure(active, code);
      return;
    }
    this.#commitTotalLength(contentRange.total);
    this.#acceptRangeBytes(active, body);
  }

  #acceptWholeHttpResponse(
    active: ActiveTicket,
    headers: BrowserHttpHeaders,
    body: ArrayBuffer,
  ): void {
    let contentLength: bigint | undefined;
    let contentRange: string | null;
    try {
      contentLength = parseU64Header(
        headers.get("content-length"),
      );
      contentRange = headers.get("content-range");
    } catch {
      discardBuffer(body);
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    if (
      contentRange !== null
      || contentLength === undefined
      || contentLength === 0n
      || contentLength
        > BigInt(this.#limits.maxWholeSourceBytes)
      || !this.#totalLengthCompatible(contentLength)
      || !rangesWithin(active.ranges, contentLength)
    ) {
      discardBuffer(body);
      this.#queueFailure(
        active,
        SourceFailureCode.InvalidRangeResponse,
      );
      return;
    }
    if (BigInt(body.byteLength) !== contentLength) {
      const code = BigInt(body.byteLength) < contentLength
        ? SourceFailureCode.Truncated
        : SourceFailureCode.InvalidRangeResponse;
      discardBuffer(body);
      this.#queueFailure(active, code);
      return;
    }
    this.#commitTotalLength(contentLength);
    if (this.#wholeSource !== undefined) {
      discardBuffer(body);
      discardBuffers(active.buffers);
      active.buffers.length = 0;
      this.#serveWholeSource(active);
      return;
    }
    this.#wholeSource = body;
    const waiting = Array.from(this.#active.values());
    for (const ticket of waiting) {
      if (ticket !== active) {
        this.#abort(ticket.controller);
      }
      discardBuffers(ticket.buffers);
      ticket.buffers.length = 0;
      this.#serveWholeSource(ticket);
    }
  }

  #acceptRangeBytes(
    active: ActiveTicket,
    bytes: ArrayBuffer,
  ): void {
    if (!this.#isActive(active)) {
      discardBuffer(bytes);
      return;
    }
    if (active.buffers.includes(bytes)) {
      discardBuffer(bytes);
      this.#queueFailure(
        active,
        SourceFailureCode.InvalidRangeResponse,
      );
      return;
    }
    active.buffers.push(bytes);
    active.nextRange += 1;
    if (active.nextRange === active.ranges.length) {
      this.#queueSuccess(active, active.buffers.slice());
    } else if (this.#source.kind === "Local") {
      this.#startLocalRead(active);
    } else {
      this.#startHttpRead(active);
    }
  }

  #serveWholeSource(active: ActiveTicket): void {
    if (!this.#isActive(active)) {
      return;
    }
    const whole = this.#wholeSource;
    if (
      whole === undefined
      || this.#knownLength === undefined
      || BigInt(whole.byteLength) !== this.#knownLength
    ) {
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    let resources: ArrayBuffer[];
    try {
      resources = active.ranges.map((range) =>
        whole.slice(
          Number(range.start),
          Number(rangeEnd(range)),
        ),
      );
    } catch {
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    if (
      resources.some(
        (resource, index) =>
          !isFixedArrayBuffer(resource)
          || BigInt(resource.byteLength)
            !== active.ranges[index]!.len,
      )
    ) {
      discardBuffers(resources);
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    this.#queueSuccess(active, resources);
  }

  #queueSuccess(
    active: ActiveTicket,
    resources: ArrayBuffer[],
  ): void {
    if (!this.#isActive(active)) {
      discardBuffers(resources);
      return;
    }
    const segments = active.ranges.map((range, slot) =>
      Object.freeze({
        range: Object.freeze({
          start: range.start,
          len: range.len,
        }),
        slot,
        byte_length: range.len,
        role: DataAttachmentRole.ImmutableRangeBytes,
      }),
    );
    const candidate: ProvideDataCommand = {
      ticket: active.ticket,
      source: snapshotSourceIdentity(this.#descriptor.identity),
      segments,
    };
    const lengths = resources.map(
      (resource) => BigInt(resource.byteLength),
    );
    if (
      resources.length === 0
      || resources.some((resource) => !isFixedArrayBuffer(resource))
      || new Set(resources).size !== resources.length
      || !validateProvideDataCommand(candidate)
      || !validateProvideDataTransferLengths(candidate, lengths)
    ) {
      discardBuffers(resources);
      this.#queueFailure(
        active,
        SourceFailureCode.TransportFailure,
      );
      return;
    }
    this.#active.delete(active.ticket);
    active.buffers.length = 0;
    const command: BrowserSourceCommand = Object.freeze({
      type: "ProvideData",
      payload: snapshotProvideDataCommand(candidate),
    });
    this.#queued.push(Object.freeze({
      ticket: active.ticket,
      worker: active.worker,
      session: active.session,
      request: active.request,
      command,
      resources: Object.freeze(resources.slice()),
      reservedBytes: active.reservedBytes,
    }));
  }

  #queueFailure(
    active: ActiveTicket,
    requestedCode: SourceFailureCode,
    requestedObserved?: SourceIdentity,
  ): void {
    if (!this.#isActive(active)) {
      return;
    }
    if (
      requestedCode === SourceFailureCode.SourceChanged
      && requestedObserved !== undefined
      && validateSourceIdentity(requestedObserved)
      && !sourceIdentityEqual(
        requestedObserved,
        this.#descriptor.identity,
      )
    ) {
      this.#queueSourceChanged(active, requestedObserved);
      return;
    }
    this.#active.delete(active.ticket);
    this.#reservedRanges.delete(active.ticket);
    this.#reservedBytes -= active.reservedBytes;
    discardBuffers(active.buffers);
    active.buffers.length = 0;

    let code = requestedCode;
    let observed: SourceIdentity | undefined;
    if (
      code === SourceFailureCode.SourceChanged
      && requestedObserved !== undefined
      && validateSourceIdentity(requestedObserved)
      && !sourceIdentityEqual(
        requestedObserved,
        this.#descriptor.identity,
      )
    ) {
      observed = snapshotSourceIdentity(requestedObserved);
    } else if (code === SourceFailureCode.SourceChanged) {
      code = SourceFailureCode.InvalidRangeResponse;
    }
    const candidate: FailDataCommand = {
      ticket: active.ticket,
      expected: snapshotSourceIdentity(this.#descriptor.identity),
      ...(observed === undefined ? {} : { observed }),
      code,
      retryable: retryableFailure(code),
    };
    if (!validateFailDataCommand(candidate)) {
      this.#enterFaulted();
      return;
    }
    const command: BrowserSourceCommand = Object.freeze({
      type: "FailData",
      payload: snapshotFailDataCommand(candidate),
    });
    this.#queued.push(Object.freeze({
      ticket: active.ticket,
      worker: active.worker,
      session: active.session,
      request: active.request,
      command,
      resources: Object.freeze([]),
      reservedBytes: 0,
    }));
  }

  #queueSourceChanged(
    active: ActiveTicket,
    observed: SourceIdentity,
  ): void {
    const worker = active.worker;
    const session = active.session;
    const request = active.request;
    const candidate: FailDataCommand = {
      ticket: active.ticket,
      expected: snapshotSourceIdentity(this.#descriptor.identity),
      observed: snapshotSourceIdentity(observed),
      code: SourceFailureCode.SourceChanged,
      retryable: false,
    };
    this.#invalidatePending();
    this.#lifecycle = "SourceChanged";
    if (!validateFailDataCommand(candidate)) {
      this.#enterFaulted();
      return;
    }
    const command: BrowserSourceCommand = Object.freeze({
      type: "FailData",
      payload: snapshotFailDataCommand(candidate),
    });
    this.#queued.push(Object.freeze({
      ticket: active.ticket,
      worker,
      session,
      request,
      command,
      resources: Object.freeze([]),
      reservedBytes: 0,
    }));
  }

  #totalLengthCompatible(total: bigint): boolean {
    if (total === 0n || total > MAX_U64) {
      return false;
    }
    return this.#knownLength === undefined
      || this.#knownLength === total;
  }

  #commitTotalLength(total: bigint): void {
    if (this.#knownLength === undefined) {
      this.#knownLength = total;
    }
  }

  #validateQueued(queued: QueuedSubmission): boolean {
    try {
      if (queued.command.type === "ProvideData") {
        return validateProvideDataCommand(queued.command.payload)
          && validateProvideDataTransferLengths(
            queued.command.payload,
            queued.resources.map(
              (resource) => BigInt(resource.byteLength),
            ),
          )
          && queued.resources.every(isFixedArrayBuffer);
      }
      return queued.resources.length === 0
        && validateFailDataCommand(queued.command.payload);
    } catch {
      return false;
    }
  }

  #dropActive(active: ActiveTicket): void {
    if (this.#active.get(active.ticket) !== active) {
      return;
    }
    this.#active.delete(active.ticket);
    this.#reservedRanges.delete(active.ticket);
    this.#reservedBytes -= active.reservedBytes;
    discardBuffers(active.buffers);
    active.buffers.length = 0;
    this.#abort(active.controller);
  }

  #releaseQueued(queued: QueuedSubmission): void {
    this.#releaseReservation(queued);
    discardBuffers(queued.resources);
  }

  #releaseReservation(queued: QueuedSubmission): void {
    if (queued.reservedBytes !== 0) {
      this.#reservedRanges.delete(queued.ticket);
      this.#reservedBytes -= queued.reservedBytes;
    }
  }

  #abort(controller: BrowserSourceAbortHandle): void {
    try {
      controller.abort();
    } catch {
      // Epoch invalidation has already made every callback harmless.
    }
  }

  #enterFaulted(): void {
    if (this.#lifecycle === "Closed") {
      return;
    }
    const sourceChanged = this.#lifecycle === "SourceChanged";
    this.#invalidatePending();
    this.#lifecycle = sourceChanged
      ? "SourceChanged"
      : "Faulted";
  }

  #invalidatePending(): void {
    this.#epochToken = {};
    const active = Array.from(this.#active.values());
    const queued = this.#queued.splice(0);
    this.#active.clear();
    this.#reservedRanges.clear();
    this.#reservedBytes = 0;
    for (const ticket of active) {
      discardBuffers(ticket.buffers);
      ticket.buffers.length = 0;
      this.#abort(ticket.controller);
    }
    for (const result of queued) {
      discardBuffers(result.resources);
    }
    if (this.#wholeSource !== undefined) {
      discardBuffer(this.#wholeSource);
      this.#wholeSource = undefined;
    }
    this.#knownLength = this.#descriptor.length;
  }
}
