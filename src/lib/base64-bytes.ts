const BASE64_DECODE_TABLE = new Int16Array(128).fill(-1);
const BASE64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

for (let i = 0; i < BASE64_ALPHABET.length; i += 1) {
  BASE64_DECODE_TABLE[BASE64_ALPHABET.charCodeAt(i)] = i;
}

function decodeBase64Char(code: number): number {
  const value =
    code >= 0 && code < BASE64_DECODE_TABLE.length ? (BASE64_DECODE_TABLE[code] ?? -1) : -1;
  if (value < 0) {
    throw new Error("malformed base64");
  }
  return value;
}

export function decodeBase64Bytes(input: string): Uint8Array {
  const clean = input.replace(/[\t\n\f\r ]+/g, "");
  if (clean.length === 0) return new Uint8Array(0);
  if (clean.length % 4 !== 0) {
    throw new Error("malformed base64 length");
  }

  let padding = 0;
  if (clean.endsWith("==")) {
    padding = 2;
  } else if (clean.endsWith("=")) {
    padding = 1;
  }

  const firstPadding = clean.indexOf("=");
  if (firstPadding !== -1 && firstPadding < clean.length - padding) {
    throw new Error("malformed base64 padding");
  }

  const output = new Uint8Array((clean.length / 4) * 3 - padding);
  let outputIndex = 0;

  for (let i = 0; i < clean.length; i += 4) {
    const isLastGroup = i === clean.length - 4;
    const c0 = decodeBase64Char(clean.charCodeAt(i));
    const c1 = decodeBase64Char(clean.charCodeAt(i + 1));
    const pad2 = clean.charCodeAt(i + 2) === 61;
    const pad3 = clean.charCodeAt(i + 3) === 61;

    if ((pad2 || pad3) && !isLastGroup) {
      throw new Error("malformed base64 padding");
    }
    if (pad2 && !pad3) {
      throw new Error("malformed base64 padding");
    }

    const c2 = pad2 ? 0 : decodeBase64Char(clean.charCodeAt(i + 2));
    const c3 = pad3 ? 0 : decodeBase64Char(clean.charCodeAt(i + 3));
    const combined = (c0 << 18) | (c1 << 12) | (c2 << 6) | c3;

    output[outputIndex] = (combined >> 16) & 0xff;
    outputIndex += 1;
    if (!pad2) {
      output[outputIndex] = (combined >> 8) & 0xff;
      outputIndex += 1;
    }
    if (!pad3) {
      output[outputIndex] = combined & 0xff;
      outputIndex += 1;
    }
  }

  return output;
}
