import { useState } from "react";
import AppBar from "@mui/material/AppBar";
import BottomNavigation from "@mui/material/BottomNavigation";
import BottomNavigationAction from "@mui/material/BottomNavigationAction";
import Box from "@mui/material/Box";
import Drawer from "@mui/material/Drawer";
import List from "@mui/material/List";
import ListItemButton from "@mui/material/ListItemButton";
import ListItemIcon from "@mui/material/ListItemIcon";
import ListItemText from "@mui/material/ListItemText";
import Paper from "@mui/material/Paper";
import Toolbar from "@mui/material/Toolbar";
import Typography from "@mui/material/Typography";
import { useTheme } from "@mui/material/styles";
import useMediaQuery from "@mui/material/useMediaQuery";
import PlayArrowIcon from "@mui/icons-material/PlayArrow";
import AppsIcon from "@mui/icons-material/Apps";
import BarChartIcon from "@mui/icons-material/BarChart";
import SettingsIcon from "@mui/icons-material/Settings";
import BugReportIcon from "@mui/icons-material/BugReport";
import HealthAndSafetyIcon from "@mui/icons-material/HealthAndSafety";
import { DashboardPage } from "./pages/DashboardPage";
import { EntriesPage } from "./pages/EntriesPage";
import { UsagePage } from "./pages/UsagePage";
import { AdminPage } from "./pages/AdminPage";
import { WindowsPage } from "./pages/WindowsPage";
import { DiagnosticsPage } from "./pages/DiagnosticsPage";

type Page = "dashboard" | "entries" | "usage" | "admin" | "health" | "windows";

const NAV: { id: Page; label: string; Icon: React.ElementType }[] = [
  { id: "dashboard", label: "Now", Icon: PlayArrowIcon },
  { id: "entries", label: "Activities", Icon: AppsIcon },
  { id: "usage", label: "Usage", Icon: BarChartIcon },
  { id: "admin", label: "Admin", Icon: SettingsIcon },
  { id: "health", label: "Health", Icon: HealthAndSafetyIcon },
  { id: "windows", label: "Windows", Icon: BugReportIcon },
];

const DRAWER_WIDTH = 200;

export function App() {
  const [page, setPage] = useState<Page>("dashboard");
  const theme = useTheme();
  const isDesktop = useMediaQuery(theme.breakpoints.up("sm"));

  const content = (
    <>
      {page === "dashboard" && (
        <DashboardPage onShowHealth={() => setPage("health")} />
      )}
      {page === "entries" && <EntriesPage />}
      {page === "usage" && <UsagePage />}
      {page === "admin" && <AdminPage />}
      {page === "health" && <DiagnosticsPage />}
      {page === "windows" && <WindowsPage />}
    </>
  );

  if (isDesktop) {
    return (
      <Box sx={{ display: "flex", minHeight: "100dvh" }}>
        <Drawer
          variant="permanent"
          sx={{
            width: DRAWER_WIDTH,
            flexShrink: 0,
            "& .MuiDrawer-paper": {
              width: DRAWER_WIDTH,
              boxSizing: "border-box",
              borderRight: "1px solid",
              borderColor: "divider",
            },
          }}
        >
          <Toolbar sx={{ px: 2 }}>
            <Typography variant="h6" color="primary" sx={{ fontWeight: 700 }}>
              Shepherd
            </Typography>
          </Toolbar>
          <List disablePadding>
            {NAV.map(({ id, label, Icon }) => (
              <ListItemButton
                key={id}
                selected={page === id}
                onClick={() => setPage(id)}
              >
                <ListItemIcon sx={{ minWidth: 36 }}>
                  <Icon color={page === id ? "primary" : "action"} />
                </ListItemIcon>
                <ListItemText primary={label} />
              </ListItemButton>
            ))}
          </List>
        </Drawer>

        <Box component="main" sx={{ flex: 1, p: 3, maxWidth: 720 }}>
          {content}
        </Box>
      </Box>
    );
  }

  return (
    <Box sx={{ display: "flex", flexDirection: "column", minHeight: "100dvh" }}>
      <AppBar position="sticky" color="inherit" elevation={0} sx={{ borderBottom: 1, borderColor: "divider" }}>
        <Toolbar>
          <Typography variant="h6" color="primary" sx={{ fontWeight: 700 }}>
            Shepherd
          </Typography>
        </Toolbar>
      </AppBar>

      <Box component="main" sx={{ flex: 1, p: 2, pb: `calc(56px + env(safe-area-inset-bottom, 0px))` }}>
        {content}
      </Box>

      <Paper
        component="nav"
        elevation={3}
        sx={{ position: "fixed", bottom: 0, left: 0, right: 0, zIndex: 20 }}
      >
        <BottomNavigation
          value={page}
          onChange={(_, v) => setPage(v)}
          showLabels
          sx={{ paddingBottom: "env(safe-area-inset-bottom, 0px)" }}
        >
          {NAV.map(({ id, label, Icon }) => (
            <BottomNavigationAction
              key={id}
              value={id}
              label={label}
              icon={<Icon />}
            />
          ))}
        </BottomNavigation>
      </Paper>
    </Box>
  );
}
