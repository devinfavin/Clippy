import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AudioTrack, TrackMix } from "./types";
import { trackEffectiveGain } from "./types";

/**
 * Plays N audio tracks in sync with the main `<video>` element through the
 * WebAudio graph, with a per-track GainNode so the mixer's sliders/mutes
 * affect playback in real time.
 *
 * Two modes depending on track count:
 *
 *   Single-track: route the <video> element itself through a GainNode.
 *     No separate <audio> elements, no sync gap, no drift.
 *     The video stays "muted" from the browser's perspective (its native
 *     output is taken over by createMediaElementSource) but the GainNode
 *     feeds the AudioContext destination.
 *
 *   Multi-track: extract each track to its own cached file, build one
 *     <audio> → GainNode per track, mirror play/pause/seek from the video.
 *     Drift correction every 2 s catches any slip between elements.
 *
 * Graceful degradation:
 *   • No tracks → video plays unmuted, hook is a no-op.
 *   • Extract failure → that track is silent; others still play.
 */
export function useAudioTracks(opts: {
  videoElement: HTMLVideoElement | null;
  srcPath: string | null;
  tracks: AudioTrack[];
  mix: TrackMix;
}) {
  const { videoElement, srcPath, tracks, mix } = opts;

  const audioCtxRef = useRef<AudioContext | null>(null);
  // Single-track path: GainNode wired directly to the video element's source.
  const videoGainRef = useRef<GainNode | null>(null);
  const videoSrcRef = useRef<MediaElementAudioSourceNode | null>(null);
  // Multi-track path: one <audio> + GainNode per extracted track.
  const audiosRef = useRef<Map<number, HTMLAudioElement>>(new Map());
  const gainsRef = useRef<Map<number, GainNode>>(new Map());
  const sourcesRef = useRef<Map<number, MediaElementAudioSourceNode>>(new Map());

  const mixRef = useRef(mix);
  useEffect(() => { mixRef.current = mix; }, [mix]);

  useEffect(() => {
    if (!videoElement || !srcPath || tracks.length < 1) {
      videoElement && (videoElement.muted = false);
      return;
    }

    let cancelled = false;
    const audios = audiosRef.current;
    const gains = gainsRef.current;
    const sources = sourcesRef.current;

    let ctx = audioCtxRef.current;
    if (!ctx) {
      try {
        ctx = new AudioContext();
        audioCtxRef.current = ctx;
      } catch {
        return;
      }
    }

    // ── Single-track: wire the video element directly through a GainNode ──
    // No extraction, no separate element, no sync problem.
    if (tracks.length === 1) {
      // createMediaElementSource can only be called once per element; reuse
      // the existing source node if the video element hasn't changed.
      let vSrc = videoSrcRef.current;
      if (!vSrc) {
        try {
          vSrc = ctx.createMediaElementSource(videoElement);
          videoSrcRef.current = vSrc;
        } catch {
          videoElement.muted = false;
          return;
        }
      }
      const gain = videoGainRef.current ?? ctx.createGain();
      videoGainRef.current = gain;
      const m = mixRef.current[tracks[0].index];
      gain.gain.value = m ? (m.muted ? 0 : m.volume) : 1;
      vSrc.connect(gain).connect(ctx.destination);
      gains.set(tracks[0].index, gain);

      const onPlay = () => ctx!.resume().catch(() => {});
      videoElement.addEventListener("play", onPlay);
      return () => {
        videoElement.removeEventListener("play", onPlay);
        gains.clear();
        // Leave vSrc connected — it persists with the video element across
        // sources. Only disconnect if the video element itself is torn down.
      };
    }

    // ── Multi-track: extract each track, play via separate <audio> elements ──
    videoElement.muted = true;
    const cleanups: Array<() => void> = [];

    // Single batched extract = one ffmpeg read pass over the source instead of
    // N parallel stream-copy processes seek-thrashing the same HDD. Cached
    // tracks short-circuit inside the command. Per-track failures come back
    // as result entries with `url: null`, so the others still play.
    const trackIndices = tracks.map((t) => t.index);
    invoke<Array<{ track_index: number; url: string | null; error: string | null }>>(
      "extract_tracks_batch",
      { srcPath, trackIndices },
    )
      .then((results) => {
        if (cancelled) return;
        for (const r of results) {
          if (!r.url) {
            console.warn(`[clippy] extract_tracks_batch track ${r.track_index} failed:`, r.error);
            continue;
          }
          const audio = new Audio();
          audio.crossOrigin = "anonymous";
          audio.preload = "auto";
          audio.src = r.url;
          audios.set(r.track_index, audio);

          const src = ctx!.createMediaElementSource(audio);
          const gain = ctx!.createGain();
          const m = mixRef.current[r.track_index];
          gain.gain.value = m ? (m.muted ? 0 : m.volume) : 1;
          src.connect(gain).connect(ctx!.destination);
          sources.set(r.track_index, src);
          gains.set(r.track_index, gain);

          audio.currentTime = videoElement.currentTime;
          if (!videoElement.paused) audio.play().catch(() => {});
        }
      })
      .catch((err) => {
        console.warn(`[clippy] extract_tracks_batch failed:`, err);
      });

    const syncTime = () => {
      const vt = videoElement.currentTime;
      audios.forEach((a) => {
        if (Math.abs(a.currentTime - vt) > 0.05) a.currentTime = vt;
      });
    };
    const onPlay = () => {
      ctx!.resume().catch(() => {});
      syncTime();
      audios.forEach((a) => a.play().catch(() => {}));
    };
    const onPause = () => audios.forEach((a) => a.pause());
    const onSeeking = () => audios.forEach((a) => a.pause());
    const onSeeked = () => {
      syncTime();
      if (!videoElement.paused) audios.forEach((a) => a.play().catch(() => {}));
    };
    const onRateChange = () => {
      audios.forEach((a) => (a.playbackRate = videoElement.playbackRate));
    };
    videoElement.addEventListener("play", onPlay);
    videoElement.addEventListener("pause", onPause);
    videoElement.addEventListener("seeking", onSeeking);
    videoElement.addEventListener("seeked", onSeeked);
    videoElement.addEventListener("ratechange", onRateChange);
    cleanups.push(() => {
      videoElement.removeEventListener("play", onPlay);
      videoElement.removeEventListener("pause", onPause);
      videoElement.removeEventListener("seeking", onSeeking);
      videoElement.removeEventListener("seeked", onSeeked);
      videoElement.removeEventListener("ratechange", onRateChange);
    });

    const driftTimer = window.setInterval(() => {
      if (videoElement.paused) return;
      const vt = videoElement.currentTime;
      audios.forEach((a) => {
        if (Math.abs(a.currentTime - vt) > 0.15) a.currentTime = vt;
      });
    }, 2000);
    cleanups.push(() => window.clearInterval(driftTimer));

    return () => {
      cancelled = true;
      cleanups.forEach((fn) => fn());
      audios.forEach((a) => {
        try { a.pause(); } catch {}
        try { a.removeAttribute("src"); a.load(); } catch {}
      });
      sources.forEach((s) => { try { s.disconnect(); } catch {} });
      gains.forEach((g) => { try { g.disconnect(); } catch {} });
      audios.clear();
      sources.clear();
      gains.clear();
      videoElement.muted = false;
    };
  }, [videoElement, srcPath, tracks]);

  // Push mix changes into GainNodes (both paths share gainsRef).
  useEffect(() => {
    gainsRef.current.forEach((g, idx) => {
      const v = trackEffectiveGain(mix, idx);
      try {
        g.gain.cancelScheduledValues(0);
        g.gain.setTargetAtTime(v, audioCtxRef.current?.currentTime ?? 0, 0.02);
      } catch {
        g.gain.value = v;
      }
    });
  }, [mix]);
}
