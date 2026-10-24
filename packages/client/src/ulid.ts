import { getRandomValues } from "node:crypto";

/** Crockford's base32, as used by ULIDs. */
const ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const RANDOM_BYTES = 10;

/** `n` as `len` base32 digits, most significant first. */
function encode(n: number, len: number): string {
  let out = "";
  for (let i = 0; i < len; i++) {
    out = ALPHABET[n % 32] + out;
    n = Math.floor(n / 32);
  }
  return out;
}

/** Adds one to a big-endian number; false when it wrapped around to zero. */
function increment(bytes: Uint8Array): boolean {
  for (let i = bytes.length - 1; i >= 0; i--) {
    if (bytes[i] !== 0xff) {
      bytes[i] = bytes[i]! + 1;
      return true;
    }
    bytes[i] = 0;
  }
  return false;
}

/**
 * A ULID generator: a 48-bit millisecond timestamp and 80 random bits as 26
 * Crockford base32 characters. Within one millisecond (or if the clock goes
 * back) it increments the previous random part, so ids sort in creation order.
 */
export function monotonicUlid(clock: () => number = Date.now): () => string {
  let lastTime = -1;
  const random = new Uint8Array(RANDOM_BYTES);
  return () => {
    const now = clock();
    if (now > lastTime) {
      lastTime = now;
      getRandomValues(random);
    } else if (!increment(random)) {
      lastTime++;
      getRandomValues(random);
    }
    let tail = "";
    for (let off = 0; off < RANDOM_BYTES; off += 5) {
      let n = 0;
      for (let i = off; i < off + 5; i++) n = n * 256 + random[i]!;
      tail += encode(n, 8);
    }
    return encode(lastTime, 10) + tail;
  };
}

/** A new ULID, e.g. for a mutation's request_id. */
export const ulid: () => string = monotonicUlid();
