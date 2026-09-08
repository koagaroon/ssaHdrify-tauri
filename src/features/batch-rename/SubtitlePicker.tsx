import { flushSync } from "react-dom";
import { sanitizeForDialog } from "../../lib/dedup-helpers";

interface SubtitleChoice {
  path: string;
  name: string;
}

interface SubtitlePickerProps {
  rowId: string;
  subtitle: SubtitleChoice | null;
  choices: readonly SubtitleChoice[];
  active: boolean;
  disabled: boolean;
  label: string;
  emptyLabel: string;
  onActivate: () => void;
  onDeactivate: () => void;
  onAssign: (path: string | null) => void;
}

/** Only the focused picker mounts the complete pool; closed rows retain their label. */
export function SubtitlePicker(props: SubtitlePickerProps) {
  const activate = () => {
    if (!props.active && !props.disabled) {
      // Populate options before the browser opens its native popup or handles a key.
      flushSync(props.onActivate);
    }
  };
  const choices =
    props.active && !props.disabled ? props.choices : props.subtitle ? [props.subtitle] : [];
  return (
    <select
      name={`subtitle-picker-${props.rowId}`}
      value={props.subtitle?.path ?? ""}
      disabled={props.disabled || props.choices.length === 0}
      onPointerDown={activate}
      onFocus={activate}
      onBlur={props.onDeactivate}
      onChange={(event) => props.onAssign(event.target.value || null)}
      className={`rename-row-picker${props.subtitle ? " is-paired" : ""}`}
      aria-label={props.label}
      title={props.subtitle ? sanitizeForDialog(props.subtitle.name) : undefined}
    >
      <option value="">{props.emptyLabel}</option>
      {choices.map((subtitle) => (
        <option key={subtitle.path} value={subtitle.path}>
          {sanitizeForDialog(subtitle.name)}
        </option>
      ))}
    </select>
  );
}
