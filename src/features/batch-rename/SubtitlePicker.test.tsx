import { createElement, type ChangeEvent } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { SubtitlePicker } from "./SubtitlePicker";

function props() {
  const choices = [
    { path: "/subs/first.ass", name: "First" },
    { path: "/subs/unpaired.ass", name: "Unpaired" },
  ];
  return {
    rowId: "row",
    subtitle: choices[0]!,
    choices,
    active: false,
    disabled: false,
    label: "Subtitle",
    emptyLabel: "None",
    onActivate: vi.fn(),
    onDeactivate: vi.fn(),
    onAssign: vi.fn(),
  };
}

describe("SubtitlePicker", () => {
  it("keeps labels in closed rows and mounts the full pool only for the active row", () => {
    const choices = Array.from({ length: 500 }, (_, i) => ({
      path: `/subs/${i}.ass`,
      name: `Subtitle ${i}`,
    }));
    const html = renderToStaticMarkup(
      createElement(
        "div",
        null,
        choices.map((subtitle, i) =>
          createElement(SubtitlePicker, {
            ...props(),
            key: i,
            rowId: String(i),
            choices,
            subtitle,
            active: i === 10,
          })
        )
      )
    );
    expect(html.match(/<select/g)).toHaveLength(500);
    expect(html.match(/<option/g)).toHaveLength(499 * 2 + 501);
    expect(html).toContain("Subtitle 499");
  });

  it("activates before keyboard or pointer interaction and retains assignment and unpairing", () => {
    const input = props();
    const element = SubtitlePicker(input);
    element.props.onFocus();
    element.props.onPointerDown();
    expect(input.onActivate).toHaveBeenCalledTimes(2);
    element.props.onChange({
      target: { value: input.choices[1]!.path },
    } as ChangeEvent<HTMLSelectElement>);
    element.props.onChange({ target: { value: "" } } as ChangeEvent<HTMLSelectElement>);
    expect(input.onAssign.mock.calls).toEqual([[input.choices[1]!.path], [null]]);
    element.props.onBlur();
    expect(input.onDeactivate).toHaveBeenCalledOnce();
    const active = renderToStaticMarkup(createElement(SubtitlePicker, { ...input, active: true }));
    expect(active).toContain(input.choices[1]!.path);
  });

  it("never expands a disabled picker and sanitizes visible source labels", () => {
    const input = {
      ...props(),
      active: true,
      disabled: true,
      subtitle: { path: "/subs/first.ass", name: "Safe\u202eName" },
    };
    SubtitlePicker(input).props.onFocus();
    expect(input.onActivate).not.toHaveBeenCalled();
    const html = renderToStaticMarkup(createElement(SubtitlePicker, input));
    expect(html.match(/<option/g)).toHaveLength(2);
    expect(html).toContain("SafeName");
    expect(html).not.toContain("\u202e");
  });
});
