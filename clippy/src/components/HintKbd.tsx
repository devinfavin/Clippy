import { formatKeybind, type Keybind } from "../keybinds";

export function HintKbd(props: {
  bind: Keybind;
  secondaryBind?: Keybind;
  label: string;
  onClick: () => void;
}) {
  return (
    <span className="hint-item" onClick={props.onClick}>
      <kbd>{formatKeybind(props.bind)}</kbd>
      {props.secondaryBind && (
        <>
          /<kbd>{formatKeybind(props.secondaryBind)}</kbd>
        </>
      )}{" "}
      {props.label}
    </span>
  );
}
