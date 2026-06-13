import React, { useCallback, useLayoutEffect, useRef } from "react";
import type { TerminalLayoutNode, SplitDirection } from "../workbench/types";
import type { TerminalClient } from "./useTerminalClients";

export function TerminalSplitView({
  node,
  activeLeafId,
  onActivate,
  onResize,
  clients,
}: {
  node: TerminalLayoutNode;
  activeLeafId: string | null;
  onActivate: (leafId: string, terminalId: string) => void;
  onResize: (splitId: string, ratio: number) => void;
  clients: React.MutableRefObject<Map<string, TerminalClient>>;
}) {
  if (node.kind === "leaf") {
    return (
      <TerminalLeaf
        leaf={node}
        active={node.id === activeLeafId}
        onActivate={onActivate}
        clients={clients}
      />
    );
  }
  const isHorizontal = node.direction === "horizontal";
  const ratio = node.ratio;
  return (
    <div className={`wb-terminal-split ${isHorizontal ? "wb-terminal-split-horizontal" : "wb-terminal-split-vertical"}`}>
      <div className="wb-terminal-split-pane" style={{ flexBasis: `${ratio * 100}%` }}>
        <TerminalSplitView
          node={node.first}
          activeLeafId={activeLeafId}
          onActivate={onActivate}
          onResize={onResize}
          clients={clients}
        />
      </div>
      <TerminalSplitResizer direction={node.direction} onResize={(next) => onResize(node.id, next)} />
      <div className="wb-terminal-split-pane" style={{ flexBasis: `${(1 - ratio) * 100}%` }}>
        <TerminalSplitView
          node={node.second}
          activeLeafId={activeLeafId}
          onActivate={onActivate}
          onResize={onResize}
          clients={clients}
        />
      </div>
    </div>
  );
}

function TerminalSplitResizer({
  direction,
  onResize,
}: {
  direction: SplitDirection;
  onResize: (ratio: number) => void;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const onMouseDown = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      const parent = ref.current?.parentElement;
      if (!parent) return;
      const rect = parent.getBoundingClientRect();
      const onMove = (ev: MouseEvent) => {
        const nextRatio = direction === "horizontal"
          ? Math.min(0.9, Math.max(0.1, (ev.clientX - rect.left) / rect.width))
          : Math.min(0.9, Math.max(0.1, (ev.clientY - rect.top) / rect.height));
        onResize(nextRatio);
      };
      const onUp = () => {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [direction, onResize],
  );
  return (
    <div
      ref={ref}
      className={`wb-terminal-splitter ${direction === "horizontal" ? "wb-terminal-splitter-vertical" : "wb-terminal-splitter-horizontal"}`}
      onMouseDown={onMouseDown}
    />
  );
}

function TerminalLeaf({
  leaf,
  active,
  onActivate,
  clients,
}: {
  leaf: Extract<TerminalLayoutNode, { kind: "leaf" }>;
  active: boolean;
  onActivate: (leafId: string, terminalId: string) => void;
  clients: React.MutableRefObject<Map<string, TerminalClient>>;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const hostRef = useRef<HTMLDivElement | null>(null);
  const terminalId = leaf.terminalId;
  const client = clients.current.get(terminalId);
  useLayoutEffect(() => {
    const el = hostRef.current;
    if (!el) return;
    if (!client) return;
    client.attach(el);
    const ro = new ResizeObserver(() => client.fit());
    ro.observe(el);
    return () => ro.disconnect();
  }, [client, terminalId]);

  const connectionStatus = client?.connectionStatus ?? "disconnected";
  const exited = client?.status === "exited";
  const statusText = exited
    ? `Exited${client?.exitCode != null ? ` (${client.exitCode})` : ""}`
    : connectionStatus === "reconnecting"
      ? "Reconnecting..."
      : "Disconnected";
  const showStatus = exited || connectionStatus !== "connected";

  return (
    <div
      ref={containerRef}
      className={`wb-terminal-pane ${active ? "wb-terminal-pane-active" : ""}`}
      onMouseDown={() => onActivate(leaf.id, terminalId)}
    >
      <div ref={hostRef} className="wb-terminal-host" />
      {showStatus && (
        <div
          className={`wb-terminal-status wb-terminal-status-${exited ? "exited" : connectionStatus}`}
          role="status"
        >
          {statusText}
        </div>
      )}
    </div>
  );
}
