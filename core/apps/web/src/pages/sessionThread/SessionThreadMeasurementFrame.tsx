import React, { type ReactNode } from "react";
import {
  SESSION_THREAD_LAYOUT_STYLE,
} from "./sessionThreadLayoutTokens";

export function SessionThreadMeasurementFrame({
  children,
  fillHeight = false,
}: {
  children?: ReactNode;
  fillHeight?: boolean;
}) {
  return React.createElement(
    "div",
    {
      className: "wb-session-view",
      "data-testid": "session-view",
      style: {
        ...SESSION_THREAD_LAYOUT_STYLE,
        width: "100%",
        ...(fillHeight ? { height: "100%" } : {}),
      },
    },
    React.createElement(
      "div",
      { className: "wb-session-left" },
      React.createElement(
        "div",
        { className: "wb-session-slot" },
        React.createElement(
          "div",
          { className: "wb-session-slot-body" },
          children,
        ),
      ),
    ),
  );
}
