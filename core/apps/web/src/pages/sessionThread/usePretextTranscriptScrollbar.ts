import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type MutableRefObject,
  type PointerEvent as ReactPointerEvent,
} from "react";

const SCROLLBAR_HIDE_DELAY_MS = 900;

type ScrollbarDragState = {
  pointerId: number;
  startY: number;
  startScrollTop: number;
  trackHeight: number;
  thumbHeight: number;
  scrollHeight: number;
  clientHeight: number;
};

type UsePretextTranscriptScrollbarArgs = {
  containerRef: MutableRefObject<HTMLDivElement | null>;
  followBottomRef: MutableRefObject<boolean>;
};

type UsePretextTranscriptScrollbarResult = {
  scrollbarActive: boolean;
  scrollbarDragging: boolean;
  scrollbarNeeded: boolean;
  scrollbarThumbRef: MutableRefObject<HTMLDivElement | null>;
  scrollbarTrackRef: MutableRefObject<HTMLDivElement | null>;
  handleScrollbarMouseLeave: () => void;
  handleScrollbarThumbPointerDown: (event: ReactPointerEvent<HTMLDivElement>) => void;
  handleScrollbarThumbPointerMove: (event: ReactPointerEvent<HTMLDivElement>) => void;
  handleScrollbarThumbPointerUp: (event: ReactPointerEvent<HTMLDivElement>) => void;
  handleScrollbarTrackPointerDown: (event: ReactPointerEvent<HTMLDivElement>) => void;
  scheduleScrollbarUpdate: () => void;
  showScrollbarTemporarily: () => void;
};

export function usePretextTranscriptScrollbar({
  containerRef,
  followBottomRef,
}: UsePretextTranscriptScrollbarArgs): UsePretextTranscriptScrollbarResult {
  const [scrollbarNeeded, setScrollbarNeeded] = useState(false);
  const [scrollbarActive, setScrollbarActive] = useState(false);
  const [scrollbarDragging, setScrollbarDragging] = useState(false);
  const scrollbarActiveRef = useRef(false);
  const scrollbarNeededRef = useRef(false);
  const scrollbarDraggingRef = useRef(false);
  const scrollbarHideTimerRef = useRef<number | null>(null);
  const scrollbarRafRef = useRef<number | null>(null);
  const scrollbarTrackRef = useRef<HTMLDivElement | null>(null);
  const scrollbarThumbRef = useRef<HTMLDivElement | null>(null);
  const scrollbarThumbHeightRef = useRef(0);
  const scrollbarDragRef = useRef<ScrollbarDragState | null>(null);

  const setScrollbarActiveState = useCallback((next: boolean) => {
    if (scrollbarActiveRef.current === next) return;
    scrollbarActiveRef.current = next;
    setScrollbarActive(next);
  }, []);

  const setScrollbarNeededState = useCallback((next: boolean) => {
    if (scrollbarNeededRef.current === next) return;
    scrollbarNeededRef.current = next;
    setScrollbarNeeded(next);
  }, []);

  const updateScrollbar = useCallback(() => {
    const scroller = containerRef.current;
    if (!scroller) return;
    const { scrollHeight, clientHeight, scrollTop } = scroller;
    const needsScrollbar = scrollHeight > clientHeight + 1;
    setScrollbarNeededState(needsScrollbar);
    if (!needsScrollbar) return;
    const track = scrollbarTrackRef.current;
    const thumb = scrollbarThumbRef.current;
    if (!track || !thumb) return;
    const trackHeight = track.clientHeight;
    if (trackHeight <= 0 || clientHeight <= 0) return;
    const thumbHeight = Math.max((clientHeight / scrollHeight) * trackHeight, 24);
    scrollbarThumbHeightRef.current = thumbHeight;
    const maxThumbTop = Math.max(trackHeight - thumbHeight, 0);
    const maxScrollTop = Math.max(scrollHeight - clientHeight, 1);
    const thumbTop = Math.min(maxThumbTop, Math.max(0, (scrollTop / maxScrollTop) * maxThumbTop));
    thumb.style.height = `${thumbHeight}px`;
    thumb.style.transform = `translateY(${thumbTop}px)`;
  }, [containerRef, setScrollbarNeededState]);

  const scheduleScrollbarUpdate = useCallback(() => {
    if (scrollbarRafRef.current != null) return;
    scrollbarRafRef.current = window.requestAnimationFrame(() => {
      scrollbarRafRef.current = null;
      updateScrollbar();
    });
  }, [updateScrollbar]);

  const showScrollbarTemporarily = useCallback(() => {
    const scroller = containerRef.current;
    if (scroller) {
      setScrollbarNeededState(scroller.scrollHeight > scroller.clientHeight + 1);
    }
    setScrollbarActiveState(true);
    if (scrollbarHideTimerRef.current != null) {
      window.clearTimeout(scrollbarHideTimerRef.current);
    }
    scrollbarHideTimerRef.current = window.setTimeout(() => {
      if (scrollbarDraggingRef.current) return;
      setScrollbarActiveState(false);
      scrollbarHideTimerRef.current = null;
    }, SCROLLBAR_HIDE_DELAY_MS);
    updateScrollbar();
  }, [containerRef, setScrollbarActiveState, setScrollbarNeededState, updateScrollbar]);

  const handleScrollbarTrackPointerDown = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (event.button !== 0) return;
      if (event.target === scrollbarThumbRef.current) return;
      const scroller = containerRef.current;
      const track = scrollbarTrackRef.current;
      if (!scroller || !track) return;
      event.preventDefault();
      const rect = track.getBoundingClientRect();
      const ratio = Math.min(1, Math.max(0, (event.clientY - rect.top) / rect.height));
      const maxScrollTop = Math.max(scroller.scrollHeight - scroller.clientHeight, 0);
      followBottomRef.current = false;
      scroller.scrollTop = ratio * maxScrollTop;
      scheduleScrollbarUpdate();
      showScrollbarTemporarily();
    },
    [containerRef, followBottomRef, scheduleScrollbarUpdate, showScrollbarTemporarily],
  );

  const handleScrollbarThumbPointerDown = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (event.button !== 0) return;
      const scroller = containerRef.current;
      const track = scrollbarTrackRef.current;
      if (!scroller || !track) return;
      event.preventDefault();
      event.stopPropagation();
      updateScrollbar();
      const trackHeight = track.clientHeight;
      const thumbHeight = scrollbarThumbHeightRef.current;
      const maxScrollTop = scroller.scrollHeight - scroller.clientHeight;
      if (maxScrollTop <= 0 || trackHeight <= thumbHeight) return;
      if (scrollbarHideTimerRef.current != null) {
        window.clearTimeout(scrollbarHideTimerRef.current);
        scrollbarHideTimerRef.current = null;
      }
      followBottomRef.current = false;
      setScrollbarActiveState(true);
      setScrollbarDragging(true);
      scrollbarDraggingRef.current = true;
      scrollbarDragRef.current = {
        pointerId: event.pointerId,
        startY: event.clientY,
        startScrollTop: scroller.scrollTop,
        trackHeight,
        thumbHeight,
        scrollHeight: scroller.scrollHeight,
        clientHeight: scroller.clientHeight,
      };
      scrollbarThumbRef.current?.setPointerCapture(event.pointerId);
    },
    [containerRef, followBottomRef, setScrollbarActiveState, updateScrollbar],
  );

  const handleScrollbarThumbPointerMove = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const drag = scrollbarDragRef.current;
      const scroller = containerRef.current;
      if (!drag || !scroller || drag.pointerId !== event.pointerId) return;
      const maxScrollTop = Math.max(drag.scrollHeight - drag.clientHeight, 0);
      const maxThumbTop = Math.max(drag.trackHeight - drag.thumbHeight, 1);
      const delta = event.clientY - drag.startY;
      const nextScrollTop = drag.startScrollTop + (delta / maxThumbTop) * maxScrollTop;
      followBottomRef.current = false;
      scroller.scrollTop = Math.min(maxScrollTop, Math.max(0, nextScrollTop));
      scheduleScrollbarUpdate();
    },
    [containerRef, followBottomRef, scheduleScrollbarUpdate],
  );

  const handleScrollbarThumbPointerUp = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const drag = scrollbarDragRef.current;
      if (!drag || drag.pointerId !== event.pointerId) return;
      scrollbarDragRef.current = null;
      scrollbarThumbRef.current?.releasePointerCapture(event.pointerId);
      scrollbarDraggingRef.current = false;
      setScrollbarDragging(false);
      showScrollbarTemporarily();
    },
    [showScrollbarTemporarily],
  );

  const handleScrollbarMouseLeave = useCallback(() => {
    if (scrollbarDraggingRef.current) return;
    if (scrollbarHideTimerRef.current != null) {
      window.clearTimeout(scrollbarHideTimerRef.current);
      scrollbarHideTimerRef.current = null;
    }
    setScrollbarActiveState(false);
  }, [setScrollbarActiveState]);

  useEffect(() => {
    return () => {
      if (scrollbarHideTimerRef.current != null) {
        window.clearTimeout(scrollbarHideTimerRef.current);
      }
      if (scrollbarRafRef.current != null) {
        window.cancelAnimationFrame(scrollbarRafRef.current);
      }
    };
  }, []);

  return {
    scrollbarActive,
    scrollbarDragging,
    scrollbarNeeded,
    scrollbarThumbRef,
    scrollbarTrackRef,
    handleScrollbarMouseLeave,
    handleScrollbarThumbPointerDown,
    handleScrollbarThumbPointerMove,
    handleScrollbarThumbPointerUp,
    handleScrollbarTrackPointerDown,
    scheduleScrollbarUpdate,
    showScrollbarTemporarily,
  };
}
