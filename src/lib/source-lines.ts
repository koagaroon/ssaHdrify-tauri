export interface SourceLine {
  start: number;
  end: number;
  body: string;
  ending: string;
}

/** Keep physical line endings intact while treating CRLF as one separator. */
export function* iterateSourceLines(content: string): Generator<SourceLine> {
  let start = 0;
  while (start < content.length) {
    let bodyEnd = start;
    while (bodyEnd < content.length && content[bodyEnd] !== "\r" && content[bodyEnd] !== "\n") {
      bodyEnd += 1;
    }

    let end = bodyEnd;
    if (content[end] === "\r") {
      end += 1;
      if (content[end] === "\n") end += 1;
    } else if (content[end] === "\n") {
      end += 1;
    }

    yield {
      start,
      end,
      body: content.slice(start, bodyEnd),
      ending: content.slice(bodyEnd, end),
    };
    start = end;
  }
}
