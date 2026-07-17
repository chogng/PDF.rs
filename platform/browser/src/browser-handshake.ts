import type { CompatibleHandshake } from "../generated/engine-protocol.js";
import {
  negotiateHandshakeResult,
} from "../generated/engine-protocol.js";

/** Stable, content-redacted browser handshake rejection categories. */
export type BrowserHandshakeErrorCode = "InvalidHandshake";

/** Rejects a handshake without retaining or formatting untrusted fields. */
export class BrowserHandshakeError extends Error {
  readonly code: BrowserHandshakeErrorCode;

  constructor(code: BrowserHandshakeErrorCode) {
    super(code);
    this.name = "BrowserHandshakeError";
    this.code = code;
  }
}

/**
 * Validates both browser endpoint Hellos through the generated protocol owner.
 *
 * The returned object carries the generated runtime brand required by command
 * and event validation. The wrapper only maps failure to a browser-facing,
 * content-free error; it does not reimplement protocol fields or negotiation.
 */
export function negotiateBrowserHello(
  localValue: unknown,
  peerValue: unknown,
): CompatibleHandshake {
  const negotiated = negotiateHandshakeResult(localValue, peerValue);
  if (!negotiated.ok) {
    throw new BrowserHandshakeError("InvalidHandshake");
  }
  return negotiated.value;
}
