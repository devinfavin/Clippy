/** Mirror of Rust's capture::MonitorInfo. */
export type MonitorInfo = {
  hmonitor: string;
  index: number;
  label: string;
  device: string;
  primary: boolean;
  width: number;
  height: number;
};

/** Mirror of Rust's audio::AudioDevice. */
export type AudioDevice = {
  id: string;
  name: string;
  is_default: boolean;
};

/** Mirror of Rust's sysinfo::SystemInfo. */
export type SystemInfo = {
  gpu_name: string;
  gpu_vram_mb: number;
  ram_total_mb: number;
  hw_encoders: string[];
};

export type CaptureMode = "perWindow" | "monitor";
export type SaveBehavior = "auto-open" | "notify";

/** Mirror of Rust's ReplaySettings.encoder_preference. */
export type EncoderPref = "auto" | "nvenc" | "amf" | "qsv" | "software";
/** Mirror of Rust's ReplaySettings.resolution_mode kind tag. */
export type ResolutionKind = "source" | "half" | "custom";
