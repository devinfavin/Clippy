//! Buffer-internal packet types shared across capture, save, and snapshot.
//!
//! Earlier iterations of this module hosted `WindowBuffer` / `AudioPacket` /
//! `trim_video` / `trim_audio` machinery for a pre-Phase-2 design where one
//! struct owned every track's deque. That model was replaced by worker-local
//! storage (the video VecDeque lives in `run_worker`; each audio track owns
//! its own deque inside `audio::AudioCaptureHandle`), so the old types were
//! dead and have been removed. `VideoPacket` remains because it's still the
//! exchange type for encoded video chunks between worker → snapshot → save.

use std::sync::Arc;

#[derive(Clone)]
pub struct VideoPacket {
    pub data: Arc<[u8]>,
    pub pts: i64,
    pub is_keyframe: bool,
}
