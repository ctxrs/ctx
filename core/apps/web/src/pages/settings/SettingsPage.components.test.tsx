import { fireEvent, render, screen } from "@testing-library/react";
import { Toggle } from "./SettingsPage.components";

describe("Settings toggle", () => {
  it("emits the next checked value", () => {
    const onChange = vi.fn();

    render(
      <Toggle
        checked={false}
        onChange={onChange}
        ariaLabel="Telemetry"
      />,
    );

    fireEvent.click(screen.getByRole("switch", { name: "Telemetry" }));
    expect(onChange).toHaveBeenCalledWith(true);
  });
});
