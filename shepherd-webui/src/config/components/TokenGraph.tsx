/**
 * Which activities bank time toward which others.
 *
 * Token gates are configured on the target, so reading the TOML tells you what
 * unlocks *this* but never what *this* unlocks. Drawn as a graph the whole
 * arrangement is visible at once.
 *
 * Rendered as inline SVG rather than with a graph library: the layout is two
 * columns and a set of curves, and a dependency would be most of a megabyte for
 * that.
 */
import Box from "@mui/material/Box";
import Typography from "@mui/material/Typography";
import { useTheme } from "@mui/material/styles";
import type { RawConfig } from "../model/config.generated";
import type { Subject } from "../doc/patches";
import { LOAD_TIME_DEFAULTS } from "../model/field-defaults.generated";

const GROUP_PREFIX = "group:";
const ROW_HEIGHT = 34;
const COLUMN_GAP = 240;
const PADDING = 12;

interface Edge {
  from: string;
  to: string;
  ratio: number;
}

export function TokenGraph({
  config,
  highlight,
}: {
  config: RawConfig;
  /** Draw this subject's incoming edges in the accent colour. */
  highlight?: Subject;
}) {
  const theme = useTheme();

  const labelFor = (subject: string): string => {
    if (subject.startsWith(GROUP_PREFIX)) {
      const id = subject.slice(GROUP_PREFIX.length);
      const group = config.groups?.find((g) => g.id === id);
      return group ? `${group.label} (category)` : subject;
    }
    const entry = config.entries?.find((e) => e.id === subject);
    return entry?.label ?? subject;
  };

  const edges: Edge[] = [];
  for (const entry of config.entries ?? []) {
    for (const from of entry.tokens?.from ?? []) {
      edges.push({ from, to: entry.id, ratio: entry.tokens?.earn_ratio ?? LOAD_TIME_DEFAULTS.token_earn_ratio,
      });
    }
  }
  // A category can be gated too, and its gate unlocks every member at once.
  for (const group of config.groups ?? []) {
    for (const from of group.tokens?.from ?? []) {
      edges.push({
        from,
        to: `${GROUP_PREFIX}${group.id}`,
        ratio: group.tokens?.earn_ratio ?? LOAD_TIME_DEFAULTS.token_earn_ratio,
      });
    }
  }

  if (edges.length === 0) {
    return (
      <Typography variant="caption" color="text.secondary">
        No token gates configured.
      </Typography>
    );
  }

  const highlightKey = highlight
    ? highlight.kind === "group"
      ? `${GROUP_PREFIX}${highlight.id}`
      : highlight.id
    : undefined;

  const sources = [...new Set(edges.map((e) => e.from))];
  const targets = [...new Set(edges.map((e) => e.to))];

  const sourceY = (s: string) => PADDING + sources.indexOf(s) * ROW_HEIGHT + ROW_HEIGHT / 2;
  const targetY = (t: string) => PADDING + targets.indexOf(t) * ROW_HEIGHT + ROW_HEIGHT / 2;

  const height = PADDING * 2 + Math.max(sources.length, targets.length) * ROW_HEIGHT;
  const width = COLUMN_GAP * 2;

  return (
    <Box sx={{ overflowX: "auto" }}>
      <Box
        component="svg"
        viewBox={`0 0 ${width} ${height}`}
        sx={{ width: "100%", minWidth: 420, height, display: "block" }}
        role="img"
        aria-label="Token earning graph"
      >
        {edges.map((e, i) => {
          const y1 = sourceY(e.from);
          const y2 = targetY(e.to);
          const highlighted = e.to === highlightKey;
          return (
            <g key={i}>
              <path
                d={`M ${COLUMN_GAP - 8} ${y1} C ${COLUMN_GAP + 60} ${y1}, ${COLUMN_GAP + 20} ${y2}, ${COLUMN_GAP + 88} ${y2}`}
                fill="none"
                stroke={highlighted ? theme.palette.primary.main : theme.palette.divider}
                strokeWidth={highlighted ? 2 : 1.5}
              />
              {e.ratio !== 1 && (
                <text
                  x={COLUMN_GAP + 30}
                  y={(y1 + y2) / 2 - 4}
                  fontSize="10"
                  fill={theme.palette.text.secondary}
                >
                  ×{e.ratio}
                </text>
              )}
            </g>
          );
        })}

        {sources.map((s) => (
          <text
            key={s}
            x={COLUMN_GAP - 16}
            y={sourceY(s) + 4}
            textAnchor="end"
            fontSize="12"
            fill={theme.palette.text.primary}
          >
            {labelFor(s)}
          </text>
        ))}

        {targets.map((t) => (
          <text
            key={t}
            x={COLUMN_GAP + 96}
            y={targetY(t) + 4}
            fontSize="12"
            fontWeight={t === highlightKey ? 700 : 400}
            fill={
              t === highlightKey
                ? theme.palette.primary.main
                : theme.palette.text.primary
            }
          >
            {labelFor(t)}
          </text>
        ))}
      </Box>
      <Typography variant="caption" color="text.secondary">
        Time spent on the left banks toward the right.
      </Typography>
    </Box>
  );
}
