// @vitest-environment jsdom
/**
 * The sign-in screen, in a DOM (issue #156).
 *
 * The server-side rules are tested in Rust. What only a DOM can show is the
 * branching this screen does before anybody has typed anything — and getting
 * that wrong is not cosmetic:
 *
 * - A device with no password must offer *setup*, not a password box. A
 *   password box on a device with no password is a dead end with no way out
 *   short of SSH.
 * - "Approve on my phone" must appear only when a companion is actually
 *   paired. Offering it on a device with no phone is a button that can only
 *   ever time out.
 * - The password form must survive alongside it, because the phone can be
 *   flat, lost, or elsewhere.
 * - The approval screen must show the code for comparison. That comparison is
 *   the entire security property: an attacker racing the parent has a
 *   different number.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

const completeSetup = vi.fn();
const login = vi.fn();
const requestApproval = vi.fn();
const pollApproval = vi.fn();

vi.mock("../api/auth", async () => {
  const actual = await vi.importActual<typeof import("../api/auth")>("../api/auth");
  return {
    ...actual,
    completeSetup: (...args: unknown[]) => completeSetup(...args),
    login: (...args: unknown[]) => login(...args),
    requestApproval: () => requestApproval(),
    pollApproval: (...args: unknown[]) => pollApproval(...args),
  };
});

const { LoginPage } = await import("../pages/LoginPage");

beforeEach(() => {
  completeSetup.mockReset().mockResolvedValue({});
  login.mockReset().mockResolvedValue({});
  requestApproval.mockReset();
  pollApproval.mockReset();
});

afterEach(cleanup);

describe("the sign-in screen", () => {
  it("offers setup, not a password box, on a device with no password", () => {
    render(
      <LoginPage
        status={{ configured: false, companion_available: false }}
        onSignedIn={() => {}}
      />,
    );
    expect(screen.getByLabelText(/setup code/i)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^sign in$/i })).toBeNull();
  });

  it("will not submit setup until the two passwords agree", async () => {
    const user = userEvent.setup();
    render(
      <LoginPage
        status={{ configured: false, companion_available: false }}
        onSignedIn={() => {}}
      />,
    );
    await user.type(screen.getByLabelText(/setup code/i), "123456");
    await user.type(screen.getByLabelText(/new password/i), "a good password");
    await user.type(screen.getByLabelText(/confirm password/i), "a good passwrod");

    const submit = screen.getByRole("button", { name: /set password/i });
    expect(submit.hasAttribute("disabled")).toBe(true);
    expect(completeSetup).not.toHaveBeenCalled();
  });

  it("sends the code and the password once they agree", async () => {
    const user = userEvent.setup();
    const onSignedIn = vi.fn();
    render(
      <LoginPage
        status={{ configured: false, companion_available: false }}
        onSignedIn={onSignedIn}
      />,
    );
    await user.type(screen.getByLabelText(/setup code/i), "123456");
    await user.type(screen.getByLabelText(/new password/i), "a good password");
    await user.type(screen.getByLabelText(/confirm password/i), "a good password");
    await user.click(screen.getByRole("button", { name: /set password/i }));

    await waitFor(() => expect(completeSetup).toHaveBeenCalledWith("123456", "a good password"));
    await waitFor(() => expect(onSignedIn).toHaveBeenCalled());
  });

  it("does not offer the phone on a device with no companion paired", () => {
    render(
      <LoginPage
        status={{ configured: true, companion_available: false }}
        onSignedIn={() => {}}
      />,
    );
    expect(screen.getByLabelText(/password/i)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /approve on my phone/i })).toBeNull();
  });

  it("offers both doors when a companion is paired", () => {
    render(
      <LoginPage
        status={{ configured: true, companion_available: true }}
        onSignedIn={() => {}}
      />,
    );
    expect(screen.getByRole("button", { name: /^sign in$/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: /approve on my phone/i })).toBeTruthy();
  });

  it("shows the code to compare while waiting for the phone", async () => {
    const user = userEvent.setup();
    requestApproval.mockResolvedValue({
      poll_token: "secret-capability",
      code: "481302",
      expires_at: new Date().toISOString(),
    });
    pollApproval.mockResolvedValue({ state: "pending" });

    render(
      <LoginPage
        status={{ configured: true, companion_available: true }}
        onSignedIn={() => {}}
      />,
    );
    await user.click(screen.getByRole("button", { name: /approve on my phone/i }));

    // The digits, and only the digits: the polling capability is a secret and
    // must never be rendered next to them.
    await waitFor(() => expect(screen.getByText("481302")).toBeTruthy());
    expect(screen.queryByText(/secret-capability/)).toBeNull();
  });

  it("reports a lockout as a wait rather than as a wrong password", async () => {
    const user = userEvent.setup();
    const { AuthError } = await import("../api/auth");
    login.mockRejectedValue(new AuthError(429, "locked_out", "too many", 120));

    render(
      <LoginPage
        status={{ configured: true, companion_available: false }}
        onSignedIn={() => {}}
      />,
    );
    await user.type(screen.getByLabelText(/password/i), "whatever");
    await user.click(screen.getByRole("button", { name: /^sign in$/i }));

    await waitFor(() => expect(screen.getByText(/try again in 2 minutes/i)).toBeTruthy());
  });
});
