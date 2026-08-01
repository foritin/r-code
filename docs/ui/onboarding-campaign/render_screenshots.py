from pathlib import Path

from playwright.sync_api import sync_playwright


ROOT = Path(__file__).resolve().parent
URL = (ROOT / "index.html").as_uri()
NAMES = [
    "campaign-01-hero.png",
    "campaign-02-agent.png",
    "campaign-03-provider.png",
    "campaign-04-workspace.png",
    "campaign-05-launch.png",
]


def assert_bounds(page):
    value = page.evaluate(
        """
        () => {
          const r = document.querySelector('.tour').getBoundingClientRect();
          return {left:r.left, top:r.top, right:r.right, bottom:r.bottom, w:innerWidth, h:innerHeight,
                  overflow:document.documentElement.scrollWidth > innerWidth};
        }
        """
    )
    assert value["left"] >= -1 and value["top"] >= -1, value
    assert value["right"] <= value["w"] + 1 and value["bottom"] <= value["h"] + 1, value
    assert not value["overflow"], value


with sync_playwright() as playwright:
    try:
        browser = playwright.chromium.launch(headless=True)
    except Exception:
        browser = playwright.chromium.launch(channel="chrome", headless=True)

    context = browser.new_context(viewport={"width": 1440, "height": 900}, device_scale_factor=1)
    page = context.new_page()
    errors = []
    page.on("console", lambda m: errors.append(f"console {m.type}: {m.text}") if m.type == "error" else None)
    page.on("pageerror", lambda e: errors.append(f"pageerror: {e}"))
    page.goto(URL, wait_until="load")
    page.evaluate("document.fonts.ready")
    page.wait_for_function("Array.from(document.images).every(i => i.complete && i.naturalWidth > 0)")

    for index, name in enumerate(NAMES):
        if index == 4:
            page.evaluate("window.campaignTour.setSlide(2)")
            page.wait_for_timeout(550)
            page.fill("#api-key", "prototype-only-key")
            page.click("#save-key")
        page.evaluate("i => window.campaignTour.setSlide(i)", index)
        page.wait_for_timeout(600)
        assert_bounds(page)
        page.screenshot(path=str(ROOT / name), full_page=False)

    # Exact provider switching and validation.
    page.evaluate("window.campaignTour.setSlide(2)")
    page.wait_for_timeout(600)
    page.click('.provider-pick button[data-provider="OpenAI"]')
    assert page.text_content("#provider-model") == "gpt-5.6-sol"
    page.click("#save-key")
    assert page.locator("#secret-field").evaluate("e => e.classList.contains('error')")
    page.fill("#api-key", "prototype-only-key")
    page.click("#save-key")
    assert page.locator("#secret-field").evaluate("e => e.classList.contains('saved')")
    assert page.input_value("#api-key") == ""

    # Scope and permission state.
    page.evaluate("window.campaignTour.setSlide(3)")
    page.wait_for_timeout(600)
    page.click('.scope-mode button[data-scope="chat"]')
    assert page.text_content("#workspace-value") == "本地工具不可用"
    page.click('.scope-mode button[data-scope="workspace"]')
    page.click('.access-pick button[data-mode="完全访问"]')
    assert "R0–R3 自动" in page.text_content("#access-note")

    # Close and Escape.
    page.click("#close")
    assert page.locator("#dialog").is_visible()
    page.keyboard.press("Escape")
    assert page.locator("#dialog").is_hidden()

    # Horizontal swipe.
    page.evaluate("window.campaignTour.setSlide(0)")
    page.wait_for_timeout(650)
    box = page.locator("#viewport").bounding_box()
    assert box is not None
    page.mouse.move(box["x"] + box["width"] * .74, box["y"] + box["height"] * .55)
    page.mouse.down()
    page.mouse.move(box["x"] + box["width"] * .30, box["y"] + box["height"] * .55, steps=8)
    page.mouse.up()
    page.wait_for_timeout(650)
    assert page.evaluate("window.campaignTour.current") == 1

    # Header movement.
    before = page.locator(".tour").bounding_box()
    head = page.locator(".tour-header").bounding_box()
    assert before and head
    page.mouse.move(head["x"] + head["width"] * .5, head["y"] + head["height"] * .5)
    page.mouse.down()
    page.mouse.move(head["x"] + head["width"] * .5 + 28, head["y"] + head["height"] * .5 + 18, steps=5)
    page.mouse.up()
    after = page.locator(".tour").bounding_box()
    assert after and after["x"] > before["x"] + 20 and after["y"] > before["y"] + 10

    # Ready action.
    page.evaluate("window.campaignTour.markReady(); window.campaignTour.setSlide(4)")
    page.wait_for_timeout(600)
    page.click("#create-session")
    assert page.locator("#dialog").is_visible()
    assert page.text_content("#dialog-title") == "配置已确认。"
    context.close()

    compact = browser.new_context(viewport={"width": 1024, "height": 768}, reduced_motion="reduce")
    compact_page = compact.new_page()
    compact_page.goto(URL, wait_until="load")
    compact_page.wait_for_function("Array.from(document.images).every(i => i.complete && i.naturalWidth > 0)")
    for index in range(5):
        compact_page.evaluate("i => window.campaignTour.setSlide(i)", index)
        assert_bounds(compact_page)
    compact.close()
    browser.close()

if errors:
    raise SystemExit("\n".join(errors))

print("Rendered 5 campaign screens; flow and responsive checks passed.")
