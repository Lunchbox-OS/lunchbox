// @vitest-environment jsdom
/**
 * The Wi-Fi panel, in a DOM (issue #194).
 *
 * What is worth checking here is not that the list renders but the handful of
 * judgements that are wrong in a way a parent pays for:
 *
 * - **A failed join says which failure.** "Wrong password" and "no address"
 *   come from different NetworkManager reasons and need different actions;
 *   showing the first for the second has somebody retyping a password that was
 *   always right.
 * - **Joining from a browser warns first**, because it is the one action on
 *   this page that can cut the browser off.
 * - **"Save for later" is what the form leads with**, which is what the issue
 *   asks the web for.
 * - **A device with no adapter says so**, rather than showing an empty list
 *   that reads as a failed scan.
 * - **An unsupported network is shown, not hidden.** A network missing from
 *   the list reads as a device that cannot see it.
 *
 * The reason mapping itself is tested in Rust, against numbers measured off
 * real hardware. These assertions are about the panel believing them.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { SavedWifiNetwork, WifiScanView } from "../api/types";

const getWifiNetworks = vi.fn();
const getSavedWifiNetworks = vi.fn();
const saveWifiNetwork = vi.fn();
const connectWifiNetwork = vi.fn();
const forgetWifiNetwork = vi.fn();
const scanWifi = vi.fn();

vi.mock("../api/client", () => ({
  getWifiNetworks: () => getWifiNetworks(),
  getSavedWifiNetworks: () => getSavedWifiNetworks(),
  saveWifiNetwork: (request: unknown) => saveWifiNetwork(request),
  connectWifiNetwork: (id: string) => connectWifiNetwork(id),
  forgetWifiNetwork: (id: string) => forgetWifiNetwork(id),
  scanWifi: () => scanWifi(),
}));

const { WifiPanel } = await import("./WifiPanel");

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function aScan(overrides: Partial<WifiScanView> = {}): WifiScanView {
  return {
    supported: true,
    radio_enabled: true,
    networks: [
      {
        ssid: "home",
        security: "wpa_psk",
        signal_percent: 82,
        bands_ghz: [2, 5],
        saved: false,
        active: false,
      },
    ],
    truncated: false,
    last_scan_age_s: 4,
    join: { state: "idle" },
    can_configure: true,
    ...overrides,
  };
}

function renderPanel() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <WifiPanel />
    </QueryClientProvider>,
  );
}

describe("the Wi-Fi panel", () => {
  it("tells a wrong password apart from a network that gave no address", async () => {
    // The distinction the whole reason mapping exists for. Both are "it did
    // not work"; only one of them is about the password.
    getWifiNetworks.mockResolvedValue(
      aScan({
        join: {
          state: "failed",
          ssid: "home",
          reason: { kind: "no_address", detail: null },
        },
      }),
    );
    getSavedWifiNetworks.mockResolvedValue([]);

    renderPanel();

    const message = await screen.findByText(
      /never gave this device an address/i,
    );
    expect(message).toBeTruthy();
    expect(message.textContent).toMatch(/DHCP/i);
    expect(screen.queryByText(/password.*was refused/i)).toBeNull();
  });

  it("says a refused password was refused", async () => {
    getWifiNetworks.mockResolvedValue(
      aScan({
        join: {
          state: "failed",
          ssid: "home",
          reason: { kind: "wrong_password", detail: null },
        },
      }),
    );
    getSavedWifiNetworks.mockResolvedValue([]);

    renderPanel();

    expect(
      await screen.findByText(/The password for home was refused/i),
    ).toBeTruthy();
  });

  it("explains a network that was not found, including the hidden case", async () => {
    // A hidden network saved without its flag fails exactly this way, and the
    // remedy is the manual form rather than trying again.
    getWifiNetworks.mockResolvedValue(
      aScan({
        join: {
          state: "failed",
          ssid: "quiet",
          reason: { kind: "not_found", detail: null },
        },
      }),
    );
    getSavedWifiNetworks.mockResolvedValue([]);

    renderPanel();

    const message = await screen.findByText(
      /No network called quiet answered/i,
    );
    expect(message.textContent).toMatch(/hidden/i);
  });

  it("leads with save for later, and warns before connecting now", async () => {
    // Joining from a browser is the one action here that can cut the browser
    // off, so the destructive path is behind a confirmation and the safe one
    // is the default.
    getWifiNetworks.mockResolvedValue(aScan());
    getSavedWifiNetworks.mockResolvedValue([]);
    saveWifiNetwork.mockResolvedValue({
      id: "uuid",
      ssid: "home",
      security: "wpa_psk",
      hidden: false,
      autoconnect: true,
      active: false,
    });
    const user = userEvent.setup();

    renderPanel();
    await user.click(await screen.findByRole("button", { name: "Join" }));

    // Both offered, with the non-destructive one the filled default.
    const save = await screen.findByRole("button", { name: /Save for later/i });
    expect(save).toBeTruthy();
    expect(screen.getByRole("button", { name: /Connect now/i })).toBeTruthy();
    // No warning until "Connect now" is chosen.
    expect(screen.queryByText(/stop responding/i)).toBeNull();

    await user.type(screen.getByLabelText("Network password"), "correcthorse");
    await user.click(screen.getByRole("button", { name: /Connect now/i }));

    expect(
      await screen.findByText(/This page may stop responding/i),
    ).toBeTruthy();
    // And nothing has been sent yet: the confirmation is a real gate.
    expect(saveWifiNetwork).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: /Connect anyway/i }));
    await waitFor(() =>
      expect(saveWifiNetwork).toHaveBeenCalledWith(
        expect.objectContaining({ ssid: "home", connect: true }),
      ),
    );
  });

  it("leaves a join's progress to the daemon, so it cannot outlive the join", async () => {
    // Seen in a browser: the panel's own "Connecting to home…" stayed on
    // screen under the daemon's "Connected to home." until somebody closed it.
    getWifiNetworks.mockResolvedValue(aScan());
    getSavedWifiNetworks.mockResolvedValue([]);
    saveWifiNetwork.mockResolvedValue({
      id: "uuid",
      ssid: "home",
      security: "wpa_psk",
      hidden: false,
      autoconnect: true,
      active: true,
    });
    const user = userEvent.setup();

    renderPanel();
    await user.click(await screen.findByRole("button", { name: "Join" }));
    await user.type(screen.getByLabelText("Network password"), "correcthorse");
    await user.click(screen.getByRole("button", { name: /Connect now/i }));
    getWifiNetworks.mockResolvedValue(
      aScan({ join: { state: "connected", ssid: "home" } }),
    );
    await user.click(screen.getByRole("button", { name: /Connect anyway/i }));

    expect(await screen.findByText("Connected to home.")).toBeTruthy();
    expect(screen.queryByText(/Connecting to home/)).toBeNull();
  });

  it("saves without connecting when asked to", async () => {
    getWifiNetworks.mockResolvedValue(aScan());
    getSavedWifiNetworks.mockResolvedValue([]);
    saveWifiNetwork.mockResolvedValue({
      id: "uuid",
      ssid: "home",
      security: "wpa_psk",
      hidden: false,
      autoconnect: true,
      active: false,
    });
    const user = userEvent.setup();

    renderPanel();
    await user.click(await screen.findByRole("button", { name: "Join" }));
    await user.type(screen.getByLabelText("Network password"), "correcthorse");
    await user.click(screen.getByRole("button", { name: /Save for later/i }));

    await waitFor(() =>
      expect(saveWifiNetwork).toHaveBeenCalledWith({
        ssid: "home",
        security: "wpa_psk",
        password: "correcthorse",
        hidden: false,
        connect: false,
      }),
    );
  });

  it("says this device has no adapter rather than showing an empty list", async () => {
    // "No Wi-Fi hardware" and "no networks in range" are different answers,
    // and showing the second for the first sends somebody walking around the
    // house with a laptop.
    getWifiNetworks.mockResolvedValue(
      aScan({ supported: false, networks: [] }),
    );
    getSavedWifiNetworks.mockResolvedValue([]);

    renderPanel();

    expect(await screen.findByText(/no Wi-Fi adapter/i)).toBeTruthy();
    expect(screen.queryByText(/No networks found yet/i)).toBeNull();
  });

  it("shows an unsupported network instead of hiding it", async () => {
    // A network missing from the list reads as a device that cannot see it.
    getWifiNetworks.mockResolvedValue(
      aScan({
        networks: [
          {
            ssid: "eduroam",
            security: "enterprise",
            signal_percent: 70,
            bands_ghz: [5],
            saved: false,
            active: false,
          },
        ],
      }),
    );
    getSavedWifiNetworks.mockResolvedValue([]);

    renderPanel();

    expect(await screen.findByText("eduroam")).toBeTruthy();
    expect(screen.getByText(/Not supported/i)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Join" })).toBeNull();
  });

  it("disables the forms on a device that cannot save a network", async () => {
    // The device can still list and still join what it knows; what it cannot
    // do is remember something new. Saying so up front beats a form that
    // accepts a password and drops it.
    getWifiNetworks.mockResolvedValue(aScan({ can_configure: false }));
    getSavedWifiNetworks.mockResolvedValue([]);

    renderPanel();

    expect(
      await screen.findByText(/This device cannot save a network/i),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "Join" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  it("offers forget on a saved network, and connect only when it is not active", async () => {
    const saved: SavedWifiNetwork[] = [
      {
        id: "uuid-active",
        ssid: "home",
        security: "wpa_psk",
        hidden: false,
        autoconnect: true,
        active: true,
      },
      {
        id: "uuid-other",
        ssid: "cafe",
        security: "open",
        hidden: false,
        autoconnect: true,
        active: false,
      },
    ];
    getWifiNetworks.mockResolvedValue(aScan({ networks: [] }));
    getSavedWifiNetworks.mockResolvedValue(saved);
    forgetWifiNetwork.mockResolvedValue(true);
    const user = userEvent.setup();

    renderPanel();

    await screen.findByText("Saved networks");
    // One Connect button, for the network that is not already active.
    expect(screen.getAllByRole("button", { name: "Connect" })).toHaveLength(1);
    const forgets = screen.getAllByRole("button", { name: "Forget" });
    expect(forgets).toHaveLength(2);

    await user.click(forgets[0]);
    await waitFor(() =>
      expect(forgetWifiNetwork).toHaveBeenCalledWith("uuid-active"),
    );
  });

  it("reports a radio that is switched off, and does not pretend it can turn it on", async () => {
    getWifiNetworks.mockResolvedValue(
      aScan({ radio_enabled: false, networks: [] }),
    );
    getSavedWifiNetworks.mockResolvedValue([]);

    renderPanel();

    const message = await screen.findByText(/radio is switched off/i);
    expect(message.textContent).toMatch(/this page cannot/i);
  });

  it("sends a manually entered network with its hidden flag", async () => {
    // Manual entry is how a hidden network is joined at all, since a hidden
    // one never appears in a scan.
    getWifiNetworks.mockResolvedValue(aScan({ networks: [] }));
    getSavedWifiNetworks.mockResolvedValue([]);
    saveWifiNetwork.mockResolvedValue({
      id: "uuid",
      ssid: "quiet",
      security: "sae",
      hidden: true,
      autoconnect: true,
      active: false,
    });
    const user = userEvent.setup();

    renderPanel();
    await user.click(
      await screen.findByRole("button", { name: /Add network/i }),
    );
    await user.type(screen.getByLabelText("Network name"), "quiet");
    await user.type(screen.getByLabelText("Network password"), "a-secret-key");
    await user.click(screen.getByRole("button", { name: /Save for later/i }));

    await waitFor(() =>
      expect(saveWifiNetwork).toHaveBeenCalledWith(
        expect.objectContaining({
          ssid: "quiet",
          hidden: true,
          connect: false,
        }),
      ),
    );
  });

  it("does not carry a password from one network into another's box", async () => {
    // Pick a network, type a key, change your mind, pick a different one --
    // the box has to be empty. A pre-filled password belonging to another
    // network is both confusing and a small secret leak across a dialog.
    getWifiNetworks.mockResolvedValue(
      aScan({
        networks: [
          {
            ssid: "home",
            security: "wpa_psk",
            signal_percent: 80,
            bands_ghz: [5],
            saved: false,
            active: false,
          },
          {
            ssid: "neighbour",
            security: "wpa_psk",
            signal_percent: 40,
            bands_ghz: [2],
            saved: false,
            active: false,
          },
        ],
      }),
    );
    getSavedWifiNetworks.mockResolvedValue([]);
    const user = userEvent.setup();

    renderPanel();
    const joins = await screen.findAllByRole("button", { name: "Join" });
    await user.click(joins[0]);
    await user.type(screen.getByLabelText("Network password"), "first-secret");
    await user.click(screen.getByRole("button", { name: "Cancel" }));

    const again = await screen.findAllByRole("button", { name: "Join" });
    await user.click(again[1]);
    const box = screen.getByLabelText("Network password") as HTMLInputElement;
    expect(box.value).toBe("");
  });

  it("shows a join in progress while one is running", async () => {
    getWifiNetworks.mockResolvedValue(
      aScan({ join: { state: "connecting", ssid: "home" } }),
    );
    getSavedWifiNetworks.mockResolvedValue([]);

    renderPanel();

    const message = await screen.findByText(/Connecting to home/i);
    // The wait is long enough that saying so is kinder than a bare spinner.
    expect(message.textContent).toMatch(/up to a minute/i);
  });
});
