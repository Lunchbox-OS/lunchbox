// @vitest-environment jsdom
/**
 * The administrators card, in a DOM (issue #149).
 *
 * What is worth checking here is not that rows render but the three judgements
 * a parent acts on, each of which is wrong in a way that matters:
 *
 * - The six digits are shown, prominently, next to the phone asking. Approving
 *   without comparing them is the whole failure mode this flow exists to
 *   prevent, so a card that buries the number has not done its job.
 * - The last administrator offers no Remove button. The device refuses that
 *   operation, and a button whose only outcome is an error teaches people to
 *   ignore errors.
 * - A device with Bluetooth management switched off renders nothing, rather
 *   than an empty roster that reads as "nobody administers this device".
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { AdminSummary, EnrolmentRequestInfo } from "../api/wire-types.generated";

const listAdmins = vi.fn();
const listEnrolmentRequests = vi.fn();
const approveEnrolmentRequest = vi.fn();
const denyEnrolmentRequest = vi.fn();
const revokeAdmin = vi.fn();

vi.mock("../api/client", () => ({
  listAdmins: () => listAdmins(),
  listEnrolmentRequests: () => listEnrolmentRequests(),
  approveEnrolmentRequest: (id: string) => approveEnrolmentRequest(id),
  denyEnrolmentRequest: (id: string) => denyEnrolmentRequest(id),
  revokeAdmin: (id: string) => revokeAdmin(id),
}));

const { AdministratorsCard } = await import("./AdministratorsCard");

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function admin(id: string, device_name: string, identity_address: string): AdminSummary {
  return {
    id,
    device_name,
    identity_address,
    bonded_at: "2026-09-07T15:11:40-04:00",
    role: "admin",
  };
}

function waiting(): EnrolmentRequestInfo {
  return {
    id: "req-1",
    code: "804038",
    device_name: "moto g power (2021)",
    peer: "64:11:A4:B0:7B:D9",
    requested_at: "2026-09-11T20:00:00-04:00",
    expires_at: "2026-09-11T20:05:00-04:00",
  };
}

function show() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <QueryClientProvider client={client}>
      <AdministratorsCard />
    </QueryClientProvider>,
  );
}

describe("the administrators card", () => {
  it("shows the digits and who is asking, so they can be compared", async () => {
    listAdmins.mockResolvedValue([admin("a", "Pixel 10a", "78:61:DF:9B:2C:8E")]);
    listEnrolmentRequests.mockResolvedValue([waiting()]);
    show();

    expect(await screen.findByText("804038")).toBeTruthy();
    expect(
      screen.getByText(/moto g power \(2021\) wants to administer this device/),
    ).toBeTruthy();
    // The address is what tells two phones with the same name apart.
    expect(screen.getByText(/64:11:A4:B0:7B:D9/)).toBeTruthy();
    expect(screen.getByText("Approve")).toBeTruthy();
    expect(screen.getByText("Not mine")).toBeTruthy();
  });

  it("offers no Remove button for the only administrator", async () => {
    listAdmins.mockResolvedValue([admin("a", "Pixel 10a", "78:61:DF:9B:2C:8E")]);
    listEnrolmentRequests.mockResolvedValue([]);
    show();

    expect(await screen.findByText("Pixel 10a")).toBeTruthy();
    expect(screen.queryByText("Remove")).toBeNull();
  });

  it("offers Remove once there is more than one", async () => {
    listAdmins.mockResolvedValue([
      admin("a", "Pixel 10a", "78:61:DF:9B:2C:8E"),
      admin("b", "moto g power (2021)", "64:11:A4:B0:7B:D9"),
    ]);
    listEnrolmentRequests.mockResolvedValue([]);
    show();

    expect(await screen.findByText("Pixel 10a")).toBeTruthy();
    await waitFor(() => expect(screen.getAllByText("Remove").length).toBe(2));
  });

  it("renders nothing when the device has no Bluetooth management", async () => {
    listAdmins.mockRejectedValue(new Error("Bluetooth management is not enabled"));
    listEnrolmentRequests.mockResolvedValue([]);
    const { container } = (() => {
      const client = new QueryClient({
        defaultOptions: { queries: { retry: false } },
      });
      return render(
        <QueryClientProvider client={client}>
          <AdministratorsCard />
        </QueryClientProvider>,
      );
    })();

    await waitFor(() => expect(container.textContent).toBe(""));
  });
});
