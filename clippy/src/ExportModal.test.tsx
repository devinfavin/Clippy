import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, within } from "@testing-library/react";
import { useState } from "react";
import { ExportModal } from "./ExportModal";
import {
  GIF_DEFAULT_RESOLUTION,
  SIZE_PRESETS,
  type Crop,
  type ExportFormat,
  type ExportMode,
  type GifResolution,
  type SizeLimit,
} from "./types";

// State-machine coverage for the export modal. Doesn't mock the backend — the
// modal is pure UI + parent-driven state, so we render it with a tiny harness
// that holds the props the parent normally owns. The "DONE WHEN" we care about
// per the plan: 1 vs N clips switches the label/mode picker; stitched+
// nonuniform-crop blocks the confirm; non-stitched stays enabled regardless.

type HarnessOverrides = Partial<{
  mode: ExportMode;
  size: SizeLimit;
  format: ExportFormat;
  normalize: boolean;
  gifResolution: GifResolution;
  sourceWidth: number;
  sourceHeight: number;
  sourceBitrateBps: number | null;
}>;

function Harness(props: {
  clips: Array<{ inSecs: number; outSecs: number; crop?: Crop; speed?: number }>;
  init?: HarnessOverrides;
  onCancel?: () => void;
  onConfirm?: () => void;
}) {
  const [mode, setMode] = useState<ExportMode>(props.init?.mode ?? "separate");
  const [size, setSize] = useState<SizeLimit>(props.init?.size ?? SIZE_PRESETS[0]);
  const [format, setFormat] = useState<ExportFormat>(props.init?.format ?? "mp4");
  const [normalize, setNormalize] = useState<boolean>(props.init?.normalize ?? false);
  const [gifResolution, setGifResolution] = useState<GifResolution>(
    props.init?.gifResolution ?? GIF_DEFAULT_RESOLUTION
  );
  return (
    <ExportModal
      clips={props.clips}
      mode={mode}
      setMode={setMode}
      size={size}
      setSize={setSize}
      format={format}
      setFormat={setFormat}
      normalize={normalize}
      setNormalize={setNormalize}
      gifResolution={gifResolution}
      setGifResolution={setGifResolution}
      sourceWidth={props.init?.sourceWidth ?? 1920}
      sourceHeight={props.init?.sourceHeight ?? 1080}
      sourceBitrateBps={props.init?.sourceBitrateBps ?? null}
      onCancel={props.onCancel ?? (() => {})}
      onConfirm={props.onConfirm ?? (() => {})}
    />
  );
}

function confirmButton(): HTMLButtonElement {
  // The footer has two buttons (Cancel + the primary confirm); pick the one
  // with class "primary" so we don't accidentally grab Cancel.
  return screen.getByRole("button", { name: /Export/i }) as HTMLButtonElement;
}

describe("ExportModal", () => {
  it("renders a single-clip confirm label when given one clip", () => {
    render(<Harness clips={[{ inSecs: 0, outSecs: 5 }]} />);
    expect(confirmButton()).toHaveTextContent(/^Export…$/);
  });

  it("renders the multi-clip confirm label for N clips in separate mode", () => {
    render(
      <Harness
        clips={[
          { inSecs: 0, outSecs: 5 },
          { inSecs: 6, outSecs: 12 },
          { inSecs: 15, outSecs: 20 },
        ]}
      />
    );
    expect(confirmButton()).toHaveTextContent(/Export 3 clips…/);
  });

  it("hides the Mode picker when there is only one clip", () => {
    render(<Harness clips={[{ inSecs: 0, outSecs: 5 }]} />);
    // The Mode segmented group is only rendered when clips.length > 1.
    expect(screen.queryByRole("radiogroup", { name: /Mode/i })).toBeNull();
  });

  it("shows the Mode picker when there are multiple clips", () => {
    render(
      <Harness
        clips={[
          { inSecs: 0, outSecs: 5 },
          { inSecs: 6, outSecs: 12 },
        ]}
      />
    );
    expect(screen.getByRole("radiogroup", { name: /Mode/i })).toBeInTheDocument();
  });

  it("disables the Stitched option when clips have mismatched crops", () => {
    render(
      <Harness
        clips={[
          { inSecs: 0, outSecs: 5, crop: { x: 0, y: 0, w: 200, h: 200 } },
          // Second clip has a different crop — stitched can't merge mixed dims.
          { inSecs: 6, outSecs: 12, crop: { x: 10, y: 10, w: 100, h: 100 } },
        ]}
      />
    );
    const modeGroup = screen.getByRole("radiogroup", { name: /Mode/i });
    const stitched = within(modeGroup).getByRole("radio", { name: /Stitched/i });
    expect(stitched).toBeDisabled();
  });

  it("keeps confirm enabled in separate mode even with mismatched crops", () => {
    // The crop-mismatch warning only blocks the *stitched* path. Separate
    // mode is fine — each clip is exported as its own file with its own crop.
    render(
      <Harness
        clips={[
          { inSecs: 0, outSecs: 5, crop: { x: 0, y: 0, w: 200, h: 200 } },
          { inSecs: 6, outSecs: 12, crop: { x: 10, y: 10, w: 100, h: 100 } },
        ]}
      />
    );
    expect(confirmButton()).not.toBeDisabled();
  });

  it("changes the confirm label when MP3 format is selected", () => {
    render(<Harness clips={[{ inSecs: 0, outSecs: 5 }]} />);
    const formatGroup = screen.getByRole("radiogroup", { name: /Format/i });
    fireEvent.click(within(formatGroup).getByRole("radio", { name: /MP3/i }));
    expect(confirmButton()).toHaveTextContent(/Export MP3…/);
  });

  it("changes the confirm label when GIF format is selected", () => {
    render(<Harness clips={[{ inSecs: 0, outSecs: 5 }]} />);
    const formatGroup = screen.getByRole("radiogroup", { name: /Format/i });
    fireEvent.click(within(formatGroup).getByRole("radio", { name: /GIF/i }));
    expect(confirmButton()).toHaveTextContent(/Export GIF…/);
  });

  it("calls onConfirm when the primary button is clicked", () => {
    const onConfirm = vi.fn();
    render(<Harness clips={[{ inSecs: 0, outSecs: 5 }]} onConfirm={onConfirm} />);
    fireEvent.click(confirmButton());
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("calls onCancel when Escape is pressed", () => {
    const onCancel = vi.fn();
    render(<Harness clips={[{ inSecs: 0, outSecs: 5 }]} onCancel={onCancel} />);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onCancel).toHaveBeenCalledTimes(1);
  });
});
