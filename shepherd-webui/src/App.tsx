import { Suspense, lazy, useMemo, useState } from "react";
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
import TuneIcon from "@mui/icons-material/Tune";
import HealthAndSafetyIcon from "@mui/icons-material/HealthAndSafety";
import WifiIcon from "@mui/icons-material/Wifi";
import FolderIcon from "@mui/icons-material/Folder";
import { DashboardPage } from "./pages/DashboardPage";
import { EntriesPage } from "./pages/EntriesPage";
import { UsagePage } from "./pages/UsagePage";
import { AdminPage } from "./pages/AdminPage";
import { WindowsPage } from "./pages/WindowsPage";
import { DiagnosticsPage } from "./pages/DiagnosticsPage";
import { NetworkPage } from "./pages/NetworkPage";
import { FilesPage } from "./pages/FilesPage";
import { TransferTray } from "./files/TransferTray";
import { UploadsProvider } from "./files/useUploads";
import { DeviceConfigSource } from "./sources/DeviceConfigSource";

// The config editor (src/config/), routed here since issue #185.
//
// Two things had to be true first, and both are now. It needed a ConfigSource
// that edits *this device's* config rather than a file on whatever computer is
// doing the browsing — that is DeviceConfigSource, over GET/PUT /api/v1/config.
// And it needed the API to be shut: before #156 a device with no static token
// and no BLE claim served the whole management surface to anyone who could
// reach the port, and a config write runs whatever `kind = { type = "process",
// command = ... }` is given.
//
// It is deliberately NOT gated any further than that. The earlier plan wanted
// privilege separation — a config write restricted to some subset of the
// credentials — and #156 settled the question the other way when it shipped
// `set_web_password` with "the caller has already proved they are the
// administrator by reaching this trait at all". A surface that already hands
// over the device's credential is not made safe by withholding a config write.
//
// Lazy, and mounted as a full-screen takeover rather than as a page inside the
// nav: the editor renders its own AppBar and tabs and wants the viewport. The
// lazy import is also what keeps its chunks and its ~950 kB wasm validator out
// of the initial load — though not out of dist/, and so not out of the daemon
// binary, which is the ~1.5 MB this tab costs.
const ConfigApp = lazy(() =>
  import("./config/ConfigApp").then((m) => ({ default: m.ConfigApp })),
);

type Page =
  | "dashboard"
  | "entries"
  | "usage"
  | "admin"
  | "network"
  | "files"
  | "health"
  | "windows"
  | "config";

const NAV: { id: Page; label: string; Icon: React.ElementType }[] = [
  { id: "dashboard", label: "Now", Icon: PlayArrowIcon },
  { id: "entries", label: "Activities", Icon: AppsIcon },
  { id: "usage", label: "Usage", Icon: BarChartIcon },
  { id: "admin", label: "Admin", Icon: SettingsIcon },
  { id: "network", label: "Network", Icon: WifiIcon },
  { id: "files", label: "Files", Icon: FolderIcon },
  { id: "health", label: "Health", Icon: HealthAndSafetyIcon },
  { id: "windows", label: "Windows", Icon: BugReportIcon },
  { id: "config", label: "Config", Icon: TuneIcon },
];

const DRAWER_WIDTH = 200;

/**
 * The shell, wrapped in the upload queue (issue #195).
 *
 * The provider is here rather than inside the Files tab so a transfer survives
 * a tab switch: a parent who starts a 2 GB video and then goes to look at
 * today's usage should come back to a progress bar rather than to nothing. The
 * tray renders nothing at all while there is nothing in flight.
 */
export function App() {
  return (
    <UploadsProvider>
      <AppShell />
      <TransferTray />
    </UploadsProvider>
  );
}

function AppShell() {
  const [page, setPage] = useState<Page>("dashboard");
  const theme = useTheme();
  const isDesktop = useMediaQuery(theme.breakpoints.up("sm"));

  // One instance for the life of the app: the editor holds the document's
  // ETag in the source's handle, and a source rebuilt on every render would
  // hand `openFrom` a new identity each time.
  const deviceConfig = useMemo(() => new DeviceConfigSource(), []);

  // The editor takes the whole viewport, above the drawer and the bottom nav.
  // It has its own AppBar, its own tabs and a Back control, so nesting it
  // inside this chrome would put two of each on screen — and leave the week
  // grid a phone's width of a phone's width.
  if (page === "config") {
    return (
      <Suspense
        fallback={
          <Box sx={{ display: "flex", justifyContent: "center", p: 6 }}>
            <Typography variant="body2" color="text.secondary">
              Loading the editor…
            </Typography>
          </Box>
        }
      >
        <ConfigApp
          source={deviceConfig}
          autoOpen
          onClose={() => setPage("dashboard")}
        />
      </Suspense>
    );
  }

  const content = (
    <>
      {page === "dashboard" && (
        <DashboardPage onShowHealth={() => setPage("health")} />
      )}
      {page === "entries" && <EntriesPage />}
      {page === "usage" && <UsagePage />}
      {page === "admin" && <AdminPage />}
      {page === "network" && <NetworkPage />}
      {page === "files" && <FilesPage />}
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
