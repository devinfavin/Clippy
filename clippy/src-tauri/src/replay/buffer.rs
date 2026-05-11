use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Clone)]
pub struct VideoPacket {
    pub data: Arc<[u8]>,
    pub pts: i64,
    pub is_keyframe: bool,
}

pub struct AudioPacket {
    pub data: Arc<[u8]>,
    pub pts: i64,
    pub track_idx: usize,
}

pub struct WindowBuffer {
    pub hwnd: isize,
    pub title: String,
    pub video: VecDeque<VideoPacket>,
    pub audio: Vec<VecDeque<AudioPacket>>,
    pub oldest_pts: i64,
    pub newest_pts: i64,
}

impl WindowBuffer {
    pub fn new(hwnd: isize, title: String, audio_track_count: usize) -> Self {
        Self {
            hwnd,
            title,
            video: VecDeque::new(),
            audio: (0..audio_track_count).map(|_| VecDeque::new()).collect(),
            oldest_pts: 0,
            newest_pts: 0,
        }
    }

    /// Drop video packets older than `cutoff_pts`.
    pub fn trim_video(&mut self, cutoff_pts: i64) {
        while self.video.front().map(|p| p.pts < cutoff_pts).unwrap_or(false) {
            self.video.pop_front();
        }
        if let Some(front) = self.video.front() {
            self.oldest_pts = front.pts;
        }
    }

    /// Drop audio packets older than `cutoff_pts` for all tracks.
    pub fn trim_audio(&mut self, cutoff_pts: i64) {
        for track in &mut self.audio {
            while track.front().map(|p| p.pts < cutoff_pts).unwrap_or(false) {
                track.pop_front();
            }
        }
    }

    pub fn push_video(&mut self, pkt: VideoPacket) {
        self.newest_pts = pkt.pts;
        self.video.push_back(pkt);
    }

    pub fn push_audio(&mut self, pkt: AudioPacket) {
        if let Some(track) = self.audio.get_mut(pkt.track_idx) {
            track.push_back(pkt);
        }
    }

    /// Duration buffered in seconds (100ns PTS units → seconds).
    pub fn buffered_secs(&self) -> u32 {
        if self.newest_pts <= self.oldest_pts {
            return 0;
        }
        ((self.newest_pts - self.oldest_pts) / 10_000_000) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(pts: i64, is_keyframe: bool) -> VideoPacket {
        VideoPacket {
            data: Arc::from(Vec::<u8>::new().into_boxed_slice()),
            pts,
            is_keyframe,
        }
    }

    fn ap(pts: i64, track: usize) -> AudioPacket {
        AudioPacket {
            data: Arc::from(Vec::<u8>::new().into_boxed_slice()),
            pts,
            track_idx: track,
        }
    }

    #[test]
    fn trim_video_drops_packets_strictly_before_cutoff() {
        let mut b = WindowBuffer::new(0, "t".into(), 0);
        for pts in [10, 20, 30, 40, 50] {
            b.push_video(vp(pts, false));
        }
        b.trim_video(35);
        let kept: Vec<i64> = b.video.iter().map(|p| p.pts).collect();
        assert_eq!(kept, vec![40, 50]);
        // oldest_pts must follow the front element after trim.
        assert_eq!(b.oldest_pts, 40);
    }

    #[test]
    fn trim_video_keeps_packet_exactly_at_cutoff() {
        let mut b = WindowBuffer::new(0, "t".into(), 0);
        for pts in [10, 20, 30] {
            b.push_video(vp(pts, false));
        }
        b.trim_video(20); // drop pts < 20
        let kept: Vec<i64> = b.video.iter().map(|p| p.pts).collect();
        assert_eq!(kept, vec![20, 30]);
    }

    #[test]
    fn trim_video_noop_on_empty_buffer() {
        let mut b = WindowBuffer::new(0, "t".into(), 0);
        b.trim_video(100);
        assert!(b.video.is_empty());
        assert_eq!(b.oldest_pts, 0);
    }

    #[test]
    fn trim_video_keeps_oldest_pts_when_nothing_dropped() {
        let mut b = WindowBuffer::new(0, "t".into(), 0);
        b.push_video(vp(50, true));
        b.push_video(vp(60, false));
        b.trim_video(0);
        assert_eq!(b.video.len(), 2);
    }

    #[test]
    fn trim_audio_drops_per_track_independently() {
        let mut b = WindowBuffer::new(0, "t".into(), 2);
        b.push_audio(ap(10, 0));
        b.push_audio(ap(40, 0));
        b.push_audio(ap(20, 1));
        b.push_audio(ap(50, 1));
        b.trim_audio(30);
        let t0: Vec<i64> = b.audio[0].iter().map(|p| p.pts).collect();
        let t1: Vec<i64> = b.audio[1].iter().map(|p| p.pts).collect();
        assert_eq!(t0, vec![40]);
        assert_eq!(t1, vec![50]);
    }

    #[test]
    fn push_audio_ignores_out_of_range_track() {
        let mut b = WindowBuffer::new(0, "t".into(), 1);
        b.push_audio(ap(10, 5)); // track 5 doesn't exist
        assert!(b.audio[0].is_empty());
    }
}
