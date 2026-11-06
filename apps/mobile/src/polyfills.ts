// What the shared client needs that React Native lacks. Hermes has
// TextEncoder, and Expo installs TextDecoder, URL and a streaming fetch;
// ULIDs (request ids) need crypto.getRandomValues, which neither provides.
import { getRandomValues } from "expo-crypto";

const g = globalThis as { crypto?: Partial<Crypto> };
if (typeof g.crypto?.getRandomValues !== "function") {
  g.crypto = { ...g.crypto, getRandomValues: getRandomValues as Crypto["getRandomValues"] };
}
