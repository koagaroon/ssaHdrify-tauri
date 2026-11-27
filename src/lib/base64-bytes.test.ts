import { describe, expect, it } from "vitest";
import { decodeBase64Bytes } from "./base64-bytes";

describe("decodeBase64Bytes", () => {
  it("decodes empty, unpadded, single-pad, and double-pad canonical payloads", () => {
    expect(Array.from(decodeBase64Bytes(""))).toEqual([]);
    expect(Array.from(decodeBase64Bytes("   \r\n\t "))).toEqual([]);
    expect(Array.from(decodeBase64Bytes("QQ=="))).toEqual([0x41]);
    expect(Array.from(decodeBase64Bytes("QUI="))).toEqual([0x41, 0x42]);
    expect(Array.from(decodeBase64Bytes("QUJD"))).toEqual([0x41, 0x42, 0x43]);
    expect(Array.from(decodeBase64Bytes("AAEC\nAw=="))).toEqual([0, 1, 2, 3]);
  });

  it("rejects malformed lengths, invalid alphabet characters, and misplaced padding", () => {
    expect(() => decodeBase64Bytes("A")).toThrow(/malformed base64 length/);
    expect(() => decodeBase64Bytes("ABC")).toThrow(/malformed base64 length/);
    expect(() => decodeBase64Bytes("QUJ$")).toThrow(/malformed base64/);
    expect(() => decodeBase64Bytes("QQ==QUJD")).toThrow(/malformed base64 padding/);
    expect(() => decodeBase64Bytes("QQ=A")).toThrow(/malformed base64 padding/);
    expect(() => decodeBase64Bytes("QQ=\n=")).toThrow(/malformed base64 padding/);
  });

  it("rejects non-canonical non-zero padding bits per RFC 4648", () => {
    // 'R' = 17 (0b010001) leaves low 4 bits non-zero on a 1-byte `==` group.
    expect(() => decodeBase64Bytes("QR==")).toThrow(/non-canonical base64 padding bits/);
    // 'J' = 9 (0b001001) leaves low 2 bits non-zero on a 2-byte `=` group.
    expect(() => decodeBase64Bytes("QUJ=")).toThrow(/non-canonical base64 padding bits/);
  });
});
