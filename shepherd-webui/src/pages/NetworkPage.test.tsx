// @vitest-environment jsdom
/**
 * The network page, in a DOM (issue #182).
 *
 * What is worth checking here is not that the cards render but the three
 * judgements a parent acts on, each of which is wrong in a way that sends them
 * to the wrong machine:
 *
 * - The address they should try is the one shown first. On a device running
 *   containers, `10.0.3.1` looks exactly as much like an answer as
 *   `192.168.0.139` does, and only one of them is.
 * - A web interface that is *not* serving says so, and offers no URL. An
 *   address that will refuse the connection is worse than none.
 * - "Could not read the network" and "offline" are different sentences.
 *
 * The daemon derives `reachable` and `management_urls`, and their derivation is
 * tested in `lunchbox-api`. These assertions are about the page believing them.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { NetworkStatusView, ServiceStateSnapshot } from "../api/types";

const getNetworkStatus = vi.fn();
const getServiceState = vi.fn();

vi.mock("../api/client", () => ({
  getNetworkStatus: () => getNetworkStatus(),
  getServiceState: () => getServiceState(),
}));

const { NetworkPage } = await import("./NetworkPage");

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function v4(address: string, prefix = 24) {
  return { address, prefix, family: "v4" as const };
}

/** The shape the dev device actually returns. */
function aDeviceOnWifi(): NetworkStatusView {
  return {
    connectivity: "full",
    source: "network_manager",
    truncated: false,
    management_api: {
      state: "listening",
      addr: "0.0.0.0:8080",
      port: 8080,
      error: null,
    },
    management_urls: ["http://192.168.0.139:8080"],
    interfaces: [
      {
        name: "wlan0",
        kind: "wifi",
        up: true,
        reachable: true,
        addresses: [v4("192.168.0.139")],
        gateway: "192.168.0.1",
        dns: ["192.168.0.1"],
        wifi: { ssid: "Home", signal_percent: 60, frequency_mhz: 5220 },
      },
      {
        name: "lxcbr0",
        kind: "bridge",
        up: true,
        reachable: false,
        addresses: [v4("10.0.3.1")],
        gateway: null,
        dns: [],
        wifi: null,
      },
    ],
  };
}

const NO_CHECKS: Partial<ServiceStateSnapshot> = { internet_status: [] };

async function renderPage(
  status: NetworkStatusView,
  snapshot: Partial<ServiceStateSnapshot> = NO_CHECKS,
) {
  getNetworkStatus.mockResolvedValue(status);
  getServiceState.mockResolvedValue(snapshot);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <QueryClientProvider client={client}>
      <NetworkPage />
    </QueryClientProvider>,
  );
  // The page opens on a spinner; wait for the first read to land.
  expect(await screen.findByText("Network")).toBeTruthy();
}

describe("the network page", () => {
  it("leads with the address somebody can reach, and folds the rest away", async () => {
    await renderPage(aDeviceOnWifi());

    expect(screen.getByText("Reachable from another machine")).toBeTruthy();
    expect(screen.getByText("192.168.0.139")).toBeTruthy();
    // A container bridge has an address, and it is not a way in. It stays
    // behind the disclosure rather than sitting next to the one that works.
    expect(screen.queryByText("10.0.3.1")).toBeNull();
    expect(screen.getByText("Show 1 other interface")).toBeTruthy();
  });

  it("names the wireless network and offers a URL to open", async () => {
    await renderPage(aDeviceOnWifi());

    expect(screen.getByText("Home")).toBeTruthy();
    const link = screen.getByText("http://192.168.0.139:8080");
    expect(link.getAttribute("href")).toBe("http://192.168.0.139:8080");
  });

  it("says a web interface that never bound is not serving, and offers no URL", async () => {
    // Sending somebody to an address that will refuse the connection has them
    // debugging their phone instead of the device.
    const status = aDeviceOnWifi();
    status.management_api = {
      state: "failed",
      addr: "10.147.17.8:8080",
      port: 8080,
      error: "Cannot assign requested address",
    };
    status.management_urls = [];
    await renderPage(status);

    expect(screen.getByText("The web interface is not serving")).toBeTruthy();
    expect(screen.getByText(/Cannot assign requested address/)).toBeTruthy();
    expect(screen.queryByText(/^http:\/\//)).toBeNull();
  });

  it("distinguishes a device that could not look from one that is offline", async () => {
    const status: NetworkStatusView = {
      connectivity: "unknown",
      source: "unavailable",
      truncated: false,
      management_api: { state: "disabled", addr: null, port: null, error: null },
      management_urls: [],
      interfaces: [],
    };
    await renderPage(status);

    expect(
      screen.getByText("This device could not read its own network"),
    ).toBeTruthy();
    expect(screen.getByText("Unknown")).toBeTruthy();
    expect(screen.queryByText("Offline")).toBeNull();
  });

  it("renders the connectivity checks it reads off the service snapshot", async () => {
    // They live on the snapshot and nowhere else, so this is the one assertion
    // that the page's second query is wired up at all.
    await renderPage(aDeviceOnWifi(), {
      internet_status: [
        { target: "https://example.com", available: false },
        { target: "tcp://1.1.1.1:53", available: true },
      ],
    });

    expect(screen.getByText("Connectivity checks")).toBeTruthy();
    expect(screen.getByText("https://example.com")).toBeTruthy();
    expect(screen.getByText("Unreachable")).toBeTruthy();
    expect(screen.getByText("Reachable")).toBeTruthy();
  });
});
