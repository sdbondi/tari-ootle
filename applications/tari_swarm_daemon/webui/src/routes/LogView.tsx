//  Copyright 2024 The Tari Project
//  SPDX-License-Identifier: BSD-3-Clause

import { Fragment, ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { describeError, swarmRpc } from "../api/rpc";

const LEVELS = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"] as const;
type Level = (typeof LEVELS)[number];

/** Rendering every line of a long-running node log locks the tab up. */
const MAX_LINES = 4000;
const FOLLOW_MS = 2000;

interface Line {
  n: number;
  level: Level | null;
  text: string;
}

const LEVEL_RE = /\b(ERROR|WARN|INFO|DEBUG|TRACE)\b/;
const TIME_RE = /^(\d{4}-\d{2}-\d{2}[T ]\d+:\d{2}:\d{2}[.\d]*Z?)/;
const TARGET_RE = /\[([\w:]+::[\w:]+)\]/;

function parse(body: string): Line[] {
  return body.split("\n").map((text, i) => {
    const match = LEVEL_RE.exec(text);
    return { n: i + 1, level: (match?.[1] as Level) ?? null, text };
  });
}

/** Highlights the timestamp, the log target and every search hit, without raw HTML. */
function renderText(text: string, needle: string): ReactNode {
  const parts: ReactNode[] = [];
  let rest = text;
  let key = 0;

  const time = TIME_RE.exec(rest);
  if (time) {
    parts.push(
      <span className="log-ts" key={key++}>
        {time[1]}
      </span>,
    );
    rest = rest.slice(time[1].length);
  }

  const target = TARGET_RE.exec(rest);
  let tail = rest;
  if (target && target.index >= 0) {
    parts.push(<Fragment key={key++}>{rest.slice(0, target.index)}</Fragment>);
    parts.push(
      <span className="log-target" key={key++}>
        [{target[1]}]
      </span>,
    );
    tail = rest.slice(target.index + target[0].length);
  }

  if (!needle) {
    parts.push(<Fragment key={key++}>{tail}</Fragment>);
    return parts;
  }

  const lower = tail.toLowerCase();
  const lowerNeedle = needle.toLowerCase();
  let from = 0;
  let at = lower.indexOf(lowerNeedle);
  while (at !== -1) {
    parts.push(<Fragment key={key++}>{tail.slice(from, at)}</Fragment>);
    parts.push(<mark key={key++}>{tail.slice(at, at + needle.length)}</mark>);
    from = at + needle.length;
    at = lower.indexOf(lowerNeedle, from);
  }
  parts.push(<Fragment key={key++}>{tail.slice(from)}</Fragment>);
  return parts;
}

export default function LogView() {
  const navigate = useNavigate();
  const { name } = useParams<{ name: string; format: string }>();
  const path = useMemo(() => {
    try {
      return name ? atob(name) : null;
    } catch {
      return null;
    }
  }, [name]);

  const [body, setBody] = useState<string | null>(null);
  const [fetchError, setFetchError] = useState<string | null>(null);
  // A malformed link is a property of the URL, not something to store and sync.
  const error = path === null ? "That log link is not valid." : fetchError;
  const [hidden, setHidden] = useState<Set<Level>>(() => new Set(["DEBUG", "TRACE"] as Level[]));
  const [needle, setNeedle] = useState("");
  const [wrap, setWrap] = useState(true);
  const [follow, setFollow] = useState(true);
  const bottom = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!path) {
      return;
    }
    let cancelled = false;
    let timer: number | undefined;

    const load = async () => {
      try {
        const contents = await swarmRpc("get_file", path);
        if (!cancelled) {
          setBody(contents);
          setFetchError(null);
        }
      } catch (err) {
        if (!cancelled) {
          setFetchError(describeError(err));
        }
      }
      if (!cancelled && follow) {
        timer = window.setTimeout(load, FOLLOW_MS);
      }
    };

    void load();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [path, follow]);

  const lines = useMemo(() => (body === null ? [] : parse(body)), [body]);

  const visible = useMemo(() => {
    const lowerNeedle = needle.trim().toLowerCase();
    const kept = lines.filter(
      (line) =>
        !(line.level && hidden.has(line.level)) &&
        (!lowerNeedle || line.text.toLowerCase().includes(lowerNeedle)),
    );
    return kept.length > MAX_LINES ? kept.slice(-MAX_LINES) : kept;
  }, [lines, hidden, needle]);

  useEffect(() => {
    if (follow) {
      bottom.current?.scrollIntoView({ block: "end" });
    }
  }, [visible, follow]);

  const counts = useMemo(() => {
    const tally: Record<string, number> = {};
    for (const line of lines) {
      if (line.level) {
        tally[line.level] = (tally[line.level] ?? 0) + 1;
      }
    }
    return tally;
  }, [lines]);

  const toggle = (level: Level) =>
    setHidden((current) => {
      const next = new Set(current);
      if (next.has(level)) {
        next.delete(level);
      } else {
        next.add(level);
      }
      return next;
    });

  return (
    // Fills the page area, cancelling the page padding on all four sides.
    <div style={{ display: "flex", flexDirection: "column", height: "calc(100% + 36px)", margin: -18 }}>
      <div className="logbar">
        <button className="btn sm ghost" onClick={() => navigate(-1)}>
          ← Back
        </button>
        <span className="mono truncate grow" title={path ?? ""}>
          {path?.split("/").slice(-2).join("/") ?? "unknown file"}
        </span>

        {LEVELS.map((level) => (
          <button
            key={level}
            className={`btn sm${hidden.has(level) ? " ghost" : ""}`}
            onClick={() => toggle(level)}
            title={hidden.has(level) ? `Show ${level} lines` : `Hide ${level} lines`}
          >
            <span className={`lvl-${level}`} style={{ fontWeight: 800 }}>
              {level}
            </span>
            <span className="faint mono">{counts[level] ?? 0}</span>
          </button>
        ))}

        <input
          type="search"
          placeholder="Search"
          value={needle}
          onChange={(e) => setNeedle(e.target.value)}
          style={{ width: 200 }}
        />
        <button className={`btn sm${wrap ? " primary" : ""}`} onClick={() => setWrap(!wrap)}>
          Wrap
        </button>
        <button className={`btn sm${follow ? " primary" : ""}`} onClick={() => setFollow(!follow)}>
          Follow
        </button>
      </div>

      <div className="logview">
        {error && <p className="empty">{error}</p>}
        {!error && body === null && <p className="empty">Loading…</p>}
        {!error && body !== null && !visible.length && (
          <p className="empty">
            {lines.length ? "No lines match the filters." : "This file is empty."}
          </p>
        )}
        {visible.map((line) => (
          <div className={`logline${wrap ? "" : " nowrap"}`} key={line.n}>
            <span className={`lvl lvl-${line.level ?? ""}`}>{line.level ?? ""}</span>
            <span className="txt">{renderText(line.text, needle.trim())}</span>
          </div>
        ))}
        <div ref={bottom} />
      </div>
    </div>
  );
}
