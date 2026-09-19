import CircularProgress from "@mui/material/CircularProgress";

export function Spinner({ size = 24 }: { size?: number }) {
  return <CircularProgress size={size} aria-label="Loading" />;
}
