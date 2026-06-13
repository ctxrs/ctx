import * as React from "react";
import { DISABLE_BROWSER_TEXT_ASSISTS } from "../../lib/browserTextInputBehavior";

export const TextInput = React.forwardRef<HTMLInputElement, React.ComponentPropsWithoutRef<"input">>(
  (props, ref) => <input ref={ref} {...DISABLE_BROWSER_TEXT_ASSISTS} {...props} />,
);
TextInput.displayName = "TextInput";

export const Textarea = React.forwardRef<
  HTMLTextAreaElement,
  React.ComponentPropsWithoutRef<"textarea">
>((props, ref) => <textarea ref={ref} {...DISABLE_BROWSER_TEXT_ASSISTS} {...props} />);
Textarea.displayName = "Textarea";
