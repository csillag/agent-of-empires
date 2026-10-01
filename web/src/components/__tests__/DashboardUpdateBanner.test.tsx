// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import { createMemoryRouter, MemoryRouter, RouterProvider } from "react-router-dom";

import { DashboardUpdateBanner } from "../DashboardUpdateBanner";
import * as api from "../../lib/api";
import type { ServerAbout } from "../../lib/api";

function aboutWith(webBuildId: string | null): ServerAbout {
  return {
    version: "1.0.0",
    auth_required: false,
    passphrase_enabled: false,
    auth_mode: "none",
    read_only: false,
    behind_tunnel: false,
    profile: "main",
    acp_show_tool_durations: true,
    acp_replay_events: 0,
    build_flavor: "release",
    web_build_id: webBuildId,
    sleep_inhibit: {
      prevent_sleep_enabled: false,
      currently_held: false,
      backend_available: true,
    },
  };
}

function addEntryScript(src: string) {
  const script = document.createElement("script");
  script.type = "module";
  script.src = src;
  document.head.appendChild(script);
}

function renderBanner() {
  return render(
    <MemoryRouter>
      <DashboardUpdateBanner />
    </MemoryRouter>,
  );
}

beforeEach(() => {
  addEntryScript("/assets/index-PageBuild.js");
});

afterEach(() => {
  document.head.querySelectorAll("script").forEach((s) => s.remove());
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("DashboardUpdateBanner", () => {
  it("shows the reload prompt when the server's bundle differs", async () => {
    vi.spyOn(api, "fetchAbout").mockResolvedValue(aboutWith("index-NewerBuild.js"));
    renderBanner();
    expect(await screen.findByRole("status", { name: "Dashboard update available" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Reload now" })).toBeDefined();
  });

  it("renders nothing when the bundle matches", async () => {
    vi.spyOn(api, "fetchAbout").mockResolvedValue(aboutWith("index-PageBuild.js"));
    const { container } = renderBanner();
    // Let the mount-time check settle.
    await act(async () => {});
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing when the server does not report a build id (older binary)", async () => {
    vi.spyOn(api, "fetchAbout").mockResolvedValue(aboutWith(null));
    const { container } = renderBanner();
    await act(async () => {});
    expect(container.firstChild).toBeNull();
  });

  it("checks immediately when a lazy chunk fails to load", async () => {
    const spy = vi
      .spyOn(api, "fetchAbout")
      .mockResolvedValueOnce(aboutWith("index-PageBuild.js"))
      .mockResolvedValue(aboutWith("index-NewerBuild.js"));
    renderBanner();
    await act(async () => {});
    expect(screen.queryByRole("status")).toBeNull();

    // Stale-deploy signature: Vite fires this when a dynamic import 404s.
    await act(async () => {
      window.dispatchEvent(new Event("vite:preloadError"));
    });
    expect(await screen.findByRole("status", { name: "Dashboard update available" })).toBeDefined();
    expect(spy).toHaveBeenCalledTimes(2);
  });

  // A tab left open and visible through a deploy never flips visibility, so
  // switching sessions is when it learns the bundle changed.
  it("checks again when the route changes, within the throttle", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const spy = vi
      .spyOn(api, "fetchAbout")
      .mockResolvedValueOnce(aboutWith("index-PageBuild.js"))
      .mockResolvedValue(aboutWith("index-NewerBuild.js"));
    const router = createMemoryRouter([{ path: "*", element: <DashboardUpdateBanner /> }], {
      initialEntries: ["/sessions/a"],
    });
    render(<RouterProvider router={router} />);
    await act(async () => {});
    expect(spy).toHaveBeenCalledTimes(1);

    // Inside the throttle window a switch does not hit the server.
    await act(async () => {
      await router.navigate("/sessions/b");
    });
    expect(spy).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("status")).toBeNull();

    vi.setSystemTime(Date.now() + 31_000);
    await act(async () => {
      await router.navigate("/sessions/c");
    });
    expect(await screen.findByRole("status", { name: "Dashboard update available" })).toBeDefined();
    expect(spy).toHaveBeenCalledTimes(2);
  });
});
