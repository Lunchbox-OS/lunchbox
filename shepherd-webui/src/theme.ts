import { createTheme } from "@mui/material/styles";

const theme = createTheme({
  palette: {
    primary: { main: "#2563eb" },
    error: { main: "#dc2626" },
    warning: { main: "#d97706" },
    success: { main: "#16a34a" },
    background: { default: "#f1f5f9", paper: "#ffffff" },
    text: { primary: "#0f172a", secondary: "#64748b", disabled: "#94a3b8" },
  },
  shape: { borderRadius: 12 },
  typography: {
    fontFamily: '"Roboto","Helvetica","Arial",sans-serif',
  },
  components: {
    MuiButton: {
      styleOverrides: {
        root: { textTransform: "none", fontWeight: 600 },
        sizeLarge: { minHeight: 48 },
      },
    },
    MuiCard: {
      styleOverrides: {
        root: { boxShadow: "0 4px 12px rgba(0,0,0,0.10)" },
      },
    },
  },
});

export default theme;
