import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

import { useIsCoarsePointer } from "../../hooks/useIsCoarsePointer";
import { useMobileKeyboard } from "../../hooks/useMobileKeyboard";
import { loadScrollState, restoredScrollTop, saveScrollState } from "../../lib/acpScrollState";
import { recallScroll, rememberScroll } from "../../lib/suspendedScroll";
import { anchorIsStale, autoLoadDecision, scrollRestoreDelta } from "../../lib/historyScroll";
import { promptRepinDecision } from "../../lib/promptRepin";
import { repinOnResize } from "../../lib/repinOnResize";
import { nextStick } from "../../lib/stickToBottom";

/** Stick-to-bottom, earlier-history auto-load, and PWA-reopen scroll restore
 *  for the transcript viewport. These observers own bottom-following; the
 *  viewport primitive's own auto-scroll must stay disabled. */
export function useTranscriptScroll({
  sessionId,
  canLoadEarlierHistory,
  loadEarlierHistory,
  loadingEarlierHistory,
  composerCollapsed,
  promptSeq,
  status,
  localInflight,
  active = true,
}: {
  sessionId: string;
  canLoadEarlierHistory: boolean;
  loadEarlierHistory: () => void;
  loadingEarlierHistory: boolean;
  composerCollapsed: boolean;
  /** Counts every prompt once, from any path or device; keys the submit re-pin. */
  promptSeq: number;
  /** Live socket. Replay lands before the dial, so a bump while this is not open is hydration. */
  status: "connecting" | "open" | "closed" | "error";
  /** This client has an optimistic prompt row still awaiting its server echo. */
  localInflight: boolean;
  /** Suspended views do not poll the visual viewport. */
  active?: boolean;
}) {
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const belowViewportRef = useRef<HTMLDivElement | null>(null);
  const messagesContentRef = useRef<HTMLDivElement | null>(null);
  // Sampled on scroll: by the time a ResizeObserver fires, layout has already
  // settled, so pinned-ness must be read before the resize.
  const wasAtBottomRef = useRef<boolean>(true);
  // Last scrollTop a scroll event saw or a repin wrote. A stale scroll event
  // after content grows is not an upward move, so it must not un-stick.
  const lastScrollTopRef = useRef(0);
  // Last time we sampled at the bottom. iOS fires an interim scroll during a
  // keyboard resize that clears `wasAtBottomRef` but cannot clear this.
  const lastAtBottomAtRef = useRef(0);
  const didRestoreScrollRef = useRef(false);
  const [atBottom, setAtBottom] = useState(true);
  const { keyboardOpen } = useMobileKeyboard(active);
  // Observers outlive a suspension. A hidden viewport reads as zero, so they
  // stand down, and the handoff effect owns the recorded position.
  const activeScrollRef = useRef(active);
  const isCoarse = useIsCoarsePointer();

  /** An explicit "stick again": a programmatic scroll fires no gesture, so set
   *  the stick intent directly. */
  const pinToBottom = useCallback((behavior: ScrollBehavior) => {
    const vp = viewportRef.current;
    if (!vp) return;
    wasAtBottomRef.current = true;
    lastAtBottomAtRef.current = performance.now();
    setAtBottom(true);
    // Smooth animation reports its own scroll events. An instant write does
    // not, so record the scrollTop it lands on or the next event looks like
    // the reader moved up.
    if (behavior === "smooth") {
      vp.scrollTo({ top: vp.scrollHeight, behavior });
    } else {
      vp.scrollTop = vp.scrollHeight;
      lastScrollTopRef.current = vp.scrollTop;
    }
  }, []);
  const scrollToBottom = useCallback(() => pinToBottom("smooth"), [pinToBottom]);

  // A new prompt re-engages stick-to-bottom, as the CLI does: on a fine pointer
  // the composer growing while typing can drop the pinned intent. See
  // `promptRepinDecision` for why replayed prompts do not count.
  const seenPromptSeqRef = useRef<number | null>(null);
  useEffect(() => {
    const d = promptRepinDecision({
      seen: seenPromptSeqRef.current,
      promptSeq,
      live: status === "open",
      localInflight,
    });
    seenPromptSeqRef.current = d.seen;
    if (d.pin) pinToBottom("auto");
  }, [promptSeq, status, localInflight, pinToBottom]);

  // Mirrors so the scroll effect sees the latest load wiring without re-subscribing.
  const canLoadEarlierRef = useRef(canLoadEarlierHistory);
  const loadEarlierRef = useRef(loadEarlierHistory);
  const loadingEarlierRef = useRef(loadingEarlierHistory);
  useEffect(() => {
    canLoadEarlierRef.current = canLoadEarlierHistory;
    loadEarlierRef.current = loadEarlierHistory;
    loadingEarlierRef.current = loadingEarlierHistory;
  }, [canLoadEarlierHistory, loadEarlierHistory, loadingEarlierHistory]);
  const autoLoadArmedRef = useRef(true);
  // Pre-growth scrollHeight, so the content observer can hold the read position
  // when older rows land above it.
  const pendingScrollAnchorRef = useRef<number | null>(null);
  const lastAutoLoadAtRef = useRef(0);

  const requestEarlierHistory = useCallback(() => {
    const vp = viewportRef.current;
    if (!vp || !canLoadEarlierRef.current) return;
    lastAutoLoadAtRef.current = performance.now();
    const stamped = vp.scrollHeight;
    pendingScrollAnchorRef.current = stamped;
    loadEarlierRef.current();
    // Drop an anchor the request did not use, or it would jump the viewport on
    // the next unrelated growth.
    requestAnimationFrame(() => {
      if (
        pendingScrollAnchorRef.current === stamped &&
        anchorIsStale(loadingEarlierRef.current, pendingScrollAnchorRef.current, vp.scrollHeight)
      ) {
        pendingScrollAnchorRef.current = null;
      }
    });
  }, []);

  useEffect(() => {
    const vp = viewportRef.current;
    if (vp && anchorIsStale(loadingEarlierHistory, pendingScrollAnchorRef.current, vp.scrollHeight)) {
      pendingScrollAnchorRef.current = null;
    }
  }, [loadingEarlierHistory]);

  useLayoutEffect(() => {
    const vp = viewportRef.current;
    const below = belowViewportRef.current;
    const content = messagesContentRef.current;
    if (!vp || !below) return;
    // On coarse pointers the browser fires "scroll" for programmatic and
    // resize-driven scrolls too, so the stick intent is only re-sampled during
    // a real touch/wheel gesture there.
    let gestureActive = false;
    let gestureClearTimer = 0;
    const scheduleGestureClear = () => {
      if (gestureClearTimer) window.clearTimeout(gestureClearTimer);
      gestureClearTimer = window.setTimeout(() => {
        gestureActive = false;
      }, 250);
    };
    const markGesture = () => {
      gestureActive = true;
      scheduleGestureClear();
    };
    const writeScrollTop = (top: number) => {
      vp.scrollTop = top;
      lastScrollTopRef.current = vp.scrollTop;
    };
    const sample = () => {
      // Coarse pointers only re-sample during a gesture. A repin's scroll
      // event otherwise arrives after content grew and would look unpinned.
      const userMayScroll = !isCoarse || gestureActive;
      const prevStuck = wasAtBottomRef.current;
      const next = nextStick(
        { stuck: prevStuck, lastTop: lastScrollTopRef.current },
        { scrollTop: vp.scrollTop, clientHeight: vp.clientHeight, scrollHeight: vp.scrollHeight },
        userMayScroll,
      );
      lastScrollTopRef.current = next.lastTop;
      wasAtBottomRef.current = next.stuck;
      if (next.stuck) lastAtBottomAtRef.current = performance.now();
      setAtBottom((prev) => (prev === next.stuck ? prev : next.stuck));
      // Gated on the restore having run: the mount sample must not clobber
      // the saved intent.
      if (next.stuck !== prevStuck && didRestoreScrollRef.current) {
        saveScrollState(sessionId, { stuck: next.stuck, top: vp.scrollTop });
      }
      if (gestureActive) scheduleGestureClear();
      const decision = autoLoadDecision({
        scrollTop: vp.scrollTop,
        clientHeight: vp.clientHeight,
        scrollHeight: vp.scrollHeight,
        armed: autoLoadArmedRef.current,
        canLoadEarlier: canLoadEarlierRef.current,
        now: performance.now(),
        lastLoadAt: lastAutoLoadAtRef.current,
      });
      autoLoadArmedRef.current = decision.armed;
      if (decision.fire) requestEarlierHistory();
    };
    const onScroll = () => sample();
    sample();
    vp.addEventListener("scroll", onScroll, { passive: true });
    vp.addEventListener("wheel", markGesture, { passive: true });
    vp.addEventListener("touchmove", markGesture, { passive: true });
    // Pin on every visualViewport resize frame so the transcript tracks the
    // soft keyboard animation in lockstep.
    const vv = typeof window !== "undefined" ? window.visualViewport : null;
    const onVvResize = () => {
      if (wasAtBottomRef.current) writeScrollTop(vp.scrollHeight);
    };
    vv?.addEventListener("resize", onVvResize);

    if (!didRestoreScrollRef.current) {
      didRestoreScrollRef.current = true;
      const saved = loadScrollState(sessionId);
      const stick = !saved || saved.stuck;
      wasAtBottomRef.current = stick;
      if (stick) lastAtBottomAtRef.current = performance.now();
      setAtBottom(stick);
      // Later passes catch content that lays out after paint; each rechecks the
      // current stick intent so an intervening user scroll wins.
      const applyStart = () => {
        const top = restoredScrollTop(saved, wasAtBottomRef.current, vp.scrollHeight, vp.clientHeight);
        if (top != null) writeScrollTop(top);
      };
      applyStart();
      requestAnimationFrame(() => requestAnimationFrame(applyStart));
      if (stick) window.setTimeout(applyStart, 150);
    }

    const saveScroll = () => {
      // A hidden layer's viewport reports 0. Suspended, the sampler's last
      // record is the position, the same source the handoff saves.
      const top = activeScrollRef.current ? vp.scrollTop : lastScrollTopRef.current;
      saveScrollState(sessionId, { stuck: wasAtBottomRef.current, top });
    };
    const onVisibility = () => {
      if (document.visibilityState === "hidden") saveScroll();
    };
    window.addEventListener("pagehide", saveScroll);
    document.addEventListener("visibilitychange", onVisibility);
    // The viewport itself is observed too: chrome outside this view (the App
    // header collapse) can resize it.
    const wasAtBottom = () => activeScrollRef.current && wasAtBottomRef.current;
    const repin = () => {
      writeScrollTop(vp.scrollHeight);
    };
    const ro = repinOnResize({ target: below, readHeight: () => below.offsetHeight, wasAtBottom, repin });
    const vpRo = repinOnResize({ target: vp, readHeight: () => vp.clientHeight, wasAtBottom, repin });
    // Growth with a pending anchor came from older rows at the top: keep the
    // read position. Otherwise it grew at the bottom: follow if pinned.
    const contentRo = new ResizeObserver(() => {
      if (!activeScrollRef.current) return;
      const anchor = pendingScrollAnchorRef.current;
      if (anchor != null) {
        const delta = scrollRestoreDelta(anchor, vp.scrollHeight, wasAtBottomRef.current);
        if (delta > 0) writeScrollTop(vp.scrollTop + delta);
        pendingScrollAnchorRef.current = null;
        return;
      }
      if (wasAtBottomRef.current) {
        writeScrollTop(vp.scrollHeight);
      }
    });
    if (content) contentRo.observe(content);
    return () => {
      ro.disconnect();
      vpRo.disconnect();
      contentRo.disconnect();
      vp.removeEventListener("scroll", onScroll);
      vp.removeEventListener("wheel", markGesture);
      vp.removeEventListener("touchmove", markGesture);
      vv?.removeEventListener("resize", onVvResize);
      window.removeEventListener("pagehide", saveScroll);
      document.removeEventListener("visibilitychange", onVisibility);
      saveScroll();
      if (gestureClearTimer) window.clearTimeout(gestureClearTimer);
    };
  }, [requestEarlierHistory, isCoarse, sessionId]);

  // Record the sampler's position before the view goes dark, and write it back
  // once the viewport is visible again. A hidden container reports scrollTop 0.
  useLayoutEffect(() => {
    if (active === activeScrollRef.current) return;
    activeScrollRef.current = active;
    if (!active) {
      rememberScroll(sessionId, { stuck: wasAtBottomRef.current, top: lastScrollTopRef.current });
      return;
    }
    const vp = viewportRef.current;
    if (!vp) return;
    const saved = recallScroll(sessionId);
    if (saved) wasAtBottomRef.current = saved.stuck;
    const apply = () => {
      const top = restoredScrollTop(saved, wasAtBottomRef.current, vp.scrollHeight, vp.clientHeight);
      if (top != null) {
        vp.scrollTop = top;
        lastScrollTopRef.current = vp.scrollTop;
      }
    };
    apply();
    const first = requestAnimationFrame(() => {
      setAtBottom(wasAtBottomRef.current);
      requestAnimationFrame(apply);
    });
    return () => cancelAnimationFrame(first);
  }, [active, sessionId]);

  // Hold the bottom pin through a keyboard or composer-collapse transition.
  // `wasAtBottomRef` covers sitting idle at the bottom; the timestamp covers an
  // interim resize-scroll that already cleared the ref.
  const chromeTransitionInitRef = useRef(true);
  useEffect(() => {
    if (chromeTransitionInitRef.current) {
      chromeTransitionInitRef.current = false;
      return;
    }
    const vp = viewportRef.current;
    if (!vp) return;
    const recentlyAtBottom = performance.now() - lastAtBottomAtRef.current < 1200;
    if (!wasAtBottomRef.current && !recentlyAtBottom) return;
    let raf = 0;
    const start = performance.now();
    const pin = () => {
      vp.scrollTop = vp.scrollHeight;
      lastScrollTopRef.current = vp.scrollTop;
      if (performance.now() - start < 500) raf = requestAnimationFrame(pin);
    };
    raf = requestAnimationFrame(pin);
    return () => cancelAnimationFrame(raf);
  }, [keyboardOpen, composerCollapsed]);

  return {
    viewportRef,
    belowViewportRef,
    messagesContentRef,
    atBottom,
    isCoarse,
    scrollToBottom,
    requestEarlierHistory,
  };
}
