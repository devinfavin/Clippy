import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AudioTrack, TrackMix } from "./types";
import { trackEffectiveGain } from "./types";

/**
 * Plays N audio tracks in sync with the main `<video>` element through the
 * WebAudio graph, with a per-track GainNode so the mixer's sliders/mutes
 * affect playback in real time.
 *
 * High-level shape:
 *   • Extract each audio track to its own playable file (cached) at file open.
 *   • For each, build `<audio>` → MediaElementSource → GainNode → destination.
 *   • Mute the video's native audio so we don't double-play track 0.
 *   • Mirror video's play/pause/seek/rate to every audio element.
 *   • Periodically resync if any audio drifts from video.currentTime.
 *
 * Behavior intentionally degrades gracefully:
 *   • Single-track sources skip the whole pipeline (just leaves video.muted=false).
 *   • While extraction is in flight, video plays its native audio normally.
 *   • If a track fails to extract, the others still mix; failed track is silent.
 */
export function useAudioTracks(opts: {
  videoElement: HTMLVideoElement | null;
  srcPath: string | null;
  tracks: AudioTrack[];
  mix: TrackMix;
}) {
  const { videoElement, srcPath, tracks, mix } = opts;

  // One <audio> + GainNode per track, indexed by track index.
  const audioCtxRef = useRef<AudioContext | null>(null);
  const audiosRef = useRef<Map<number, HTMLAudioElement>>(new Map());
  const gainsRef = useRef<Map<number, GainNode>>(new Map());
  const sourcesRef = useRef<Map<number, MediaElementAudioSourceNode>>(new Map());
  // Latest mix values, for the gain update effect (avoids re-attaching listeners).
  const mixRef = useRef(mix);
  useEffect(() => { mixRef.current = mix; }, [mix]);

  // Set up extraction + playback graph whenever the source or track set changes.
  useEffect(() => {
    if (!videoElement || !srcPath || tracks.length < 1) {
      // Source has no audio at all — nothing to mix.
      videoElement && (videoElement.muted = false);
      return;
    }

    let cancelled = false;
    const audios = audiosRef.current;
    const gains = gainsRef.current;
    const sources = sourcesRef.current;

    // Fresh AudioContext per source. Created lazily on first user gesture
    // (Chromium will keep it suspended otherwise — we resume after first play).
    let ctx = audioCtxRef.current;
    if (!ctx) {
      try {
        ctx = new AudioContext();
        audioCtxRef.current = ctx;
      } catch {
        return;
      }
    }

    // Mute the <video> immediately — we'll play the multi-track mix instead.
    videoElement.muted = true;

    // Extract each track in parallel, build the graph as each one resolves.
    const cleanups: Array<() => void> = [];
    for (const t of tracks) {
      invoke<string>("extract_track", { srcPath, trackIndex: t.index })
        .then((url) => {
          if (cancelled) return;
          const audio = new Audio();
          audio.crossOrigin = "anonymous";
          audio.preload = "auto";
          audio.src = url;
          audios.set(t.index, audio);

          const src = ctx!.createMediaElementSource(audio);
          const gain = ctx!.createGain();
          const m = mixRef.current[t.index];
          gain.gain.value = m ? (m.muted ? 0 : m.volume) : 1;
          src.connect(gain).connect(ctx!.destination);
          sources.set(t.index, src);
          gains.set(t.index, gain);

          // Sync this audio to the current video state.
          audio.currentTime = videoElement.currentTime;
          if (!videoElement.paused) {
            audio.play().catch(() => {});
          }
        })
        .catch((err) => {
          console.warn(`[clippy] extract_track ${t.index} failed:`, err);
        });
    }

    // Sync handlers: mirror every video state change to every audio element.
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

    // Drift correction: every 2s while playing, resync any audio that's slipped.
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
      // Tear down per-track audio elements + nodes for the next source.
      audios.forEach((a) => {
        try { a.pause(); } catch {}
        try { a.removeAttribute("src"); a.load(); } catch {}
      });
      sources.forEach((s) => { try { s.disconnect(); } catch {} });
      gains.forEach((g) => { try { g.disconnect(); } catch {} });
      audios.clear();
      sources.clear();
      gains.clear();
      // Restore native video audio for next source / single-track case.
      videoElement.muted = false;
    };
  }, [videoElement, srcPath, tracks]);

  // Push the latest mix into the GainNodes whenever it changes.
  useEffect(() => {
    gainsRef.current.forEach((g, idx) => {
      const v = trackEffectiveGain(mix, idx);
      // Smooth small ramps so slider drags don't click.
      try {
        g.gain.cancelScheduledValues(0);
        g.gain.setTargetAtTime(v, audioCtxRef.current?.currentTime ?? 0, 0.02);
      } catch {
        g.gain.value = v;
      }
    });
  }, [mix]);
}
