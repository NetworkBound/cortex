#!/usr/bin/env node
// One-off verification for the Cortex 3.0.0 release build: app opens, version
// shows correctly, a project can be opened, and a chat message can be sent.
import { chromium } from "playwright";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

const CDP_URL = "http://127.0.0.1:9223";
const outDir = join("scripts", "e2e-reports", "verify-3.0.0");

async function main() {
  await mkdir(outDir, { recursive: true });
  const results = {};

  const browser = await chromium.connectOverCDP(CDP_URL);
  const context = browser.contexts()[0];
  const page = context.pages()[0];

  page.on("pageerror", (err) => {
    results.pageErrors = results.pageErrors ?? [];
    results.pageErrors.push(String(err));
  });

  // 0. Dismiss first-run onboarding if present (fresh install).
  for (const sel of [
    'button:has-text("Skip setup")',
    'button:has-text("Skip")',
    'button:has-text("Get started")',
    'button:has-text("Close")',
    'button[aria-label="Close"]',
    '.onboarding-modal button',
  ]) {
    try {
      const el = await page.$(sel);
      if (el && (await el.isVisible())) {
        await el.click({ timeout: 2000 });
        results.onboardingDismissedVia = sel;
        await page.waitForTimeout(400);
        break;
      }
    } catch {
      /* try next selector */
    }
  }

  // 1. App open / baseline render
  await page.waitForTimeout(1000);
  await page.screenshot({ path: join(outDir, "0-baseline.png") });
  const bodyText = await page.evaluate(() => document.body.innerText.slice(0, 200));
  results.baselineRendered = bodyText.length > 0;
  results.baselineSnippet = bodyText;

  // 2. Version check via Settings modal (click the gear button directly —
  // Ctrl+, didn't reliably trigger it in the packaged build).
  await page.click('button[aria-label="Settings"]');
  await page.waitForTimeout(700);
  await page.click('text="Updates"');
  await page.waitForTimeout(400);
  await page.screenshot({ path: join(outDir, "1-settings.png") });
  const versionText = await page.evaluate(() => {
    const label = Array.from(document.querySelectorAll(".settings-microlabel")).find((n) =>
      /Installed version/i.test(n.textContent ?? ""),
    );
    return label?.parentElement?.textContent ?? null;
  });
  results.versionAreaText = versionText;
  results.versionMatches = !!versionText && versionText.includes("3.0.0");
  // close settings via the Cancel button (Escape isn't wired to this modal)
  await page.click('button:has-text("Cancel")');
  await page.waitForTimeout(400);

  // 3. Open a project — the sidebar already lists projects (no tab switch
  // needed); click the "cortex" project's name directly, not its GIT/CLAUDE
  // badge buttons.
  await page.screenshot({ path: join(outDir, "2-projects-tab.png") });
  let clicked = false;
  try {
    await page.getByText("cortex", { exact: true }).first().click({ timeout: 5000 });
    clicked = true;
  } catch (err) {
    results.projectClickError = String(err).slice(0, 300);
  }
  results.projectClicked = clicked;
  await page.waitForTimeout(1200);
  await page.screenshot({ path: join(outDir, "3-project-opened.png") });
  results.activeProjectHeader = await page.evaluate(() => {
    const h = document.querySelector("h1, h2, [class*='project-title'], [class*='ProjectTitle']");
    return h?.textContent?.trim() ?? null;
  });

  // 4. Attempt to send a chat message
  await page.evaluate(() => {
    // eslint-disable-next-line no-undef
    if (window.__cortexTabSwitch) window.__cortexTabSwitch(null);
  });
  await page.waitForTimeout(500);
  const composer = await page.evaluate(() => {
    const els = Array.from(document.querySelectorAll("textarea, div[contenteditable='true']"));
    const el = els.find((e) => {
      const ph = e.getAttribute("placeholder") || e.getAttribute("aria-label") || "";
      return /message|ask|prompt|chat/i.test(ph) || els.length === 1;
    }) ?? els[0];
    return el ? { tag: el.tagName, placeholder: el.getAttribute("placeholder"), aria: el.getAttribute("aria-label") } : null;
  });
  results.composerFound = composer;

  // Pick an explicitly-configured agent instead of "Auto" — Auto routed to a
  // local vllm server that isn't running on this box, which isn't a Cortex bug.
  const select = await page.$("select");
  if (select) {
    await select.selectOption({ label: "Claude Sonnet 4.6" });
    results.modelSelected = "Claude Sonnet 4.6";
    await page.waitForTimeout(300);
  }

  if (composer) {
    const selector = composer.tag === "TEXTAREA" ? "textarea" : "div[contenteditable='true']";
    const locator = page.locator(selector).first();
    await locator.click();
    await locator.fill("Verification ping: are you online? (Cortex 3.0.0 release check)");
    await page.screenshot({ path: join(outDir, "4-message-typed.png") });
    await page.keyboard.press("Control+Enter");
    await page.waitForTimeout(3000);
    await page.screenshot({ path: join(outDir, "5-after-send.png") });
    const afterText = await page.evaluate(() => document.body.innerText.slice(-1500));
    results.afterSendSnippet = afterText;
  }

  await writeFile(join(outDir, "report.json"), JSON.stringify(results, null, 2));
  console.log(JSON.stringify(results, null, 2));
  await browser.close();
}

main().catch((err) => {
  console.error("VERIFY SCRIPT ERROR:", err);
  process.exit(1);
});
