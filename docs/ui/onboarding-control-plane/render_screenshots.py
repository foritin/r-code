from pathlib import Path

from playwright.sync_api import sync_playwright


ROOT = Path(__file__).resolve().parent
URL = (ROOT / "index.html").as_uri()
NAMES = [
    "control-01-context.png",
    "control-02-engine.png",
    "control-03-provider.png",
    "control-04-scope.png",
]


def assert_inside_viewport(page) -> None:
    result = page.evaluate(
        """
        () => {
          const tour = document.querySelector('.tour').getBoundingClientRect();
          return {
            left: tour.left,
            top: tour.top,
            right: tour.right,
            bottom: tour.bottom,
            width: innerWidth,
            height: innerHeight,
            bodyOverflowX: document.documentElement.scrollWidth > innerWidth
          };
        }
        """
    )
    assert result["left"] >= -1 and result["top"] >= -1, result
    assert result["right"] <= result["width"] + 1, result
    assert result["bottom"] <= result["height"] + 1, result
    assert not result["bodyOverflowX"], result


with sync_playwright() as playwright:
    try:
        browser = playwright.chromium.launch(headless=True)
    except Exception:
        browser = playwright.chromium.launch(channel="chrome", headless=True)

    context = browser.new_context(
        viewport={"width": 1440, "height": 900},
        device_scale_factor=1,
        reduced_motion="no-preference",
    )
    page = context.new_page()
    errors = []
    page.on(
        "console",
        lambda message: errors.append(f"console {message.type}: {message.text}")
        if message.type == "error"
        else None,
    )
    page.on("pageerror", lambda error: errors.append(f"pageerror: {error}"))
    page.goto(URL, wait_until="load")
    page.evaluate("document.fonts.ready")
    page.wait_for_function(
        "Array.from(document.images).every(image => image.complete && image.naturalWidth > 0)"
    )
    page.wait_for_timeout(350)

    for index, name in enumerate(NAMES):
        page.evaluate("index => window.controlPlaneTour.setSlide(index)", index)
        page.wait_for_timeout(650)
        assert_inside_viewport(page)
        page.screenshot(path=str(ROOT / name), full_page=False)

    # Key interaction checks: provider values, policy snapshot, close/Escape and drag.
    page.evaluate("window.controlPlaneTour.setSlide(2)")
    page.wait_for_timeout(50)
    page.click('.provider-option[data-provider="Anthropic"]')
    assert page.text_content("#provider-model") == "claude-sonnet-5"
    assert page.text_content("#provider-auth") == "X-API-Key"
    page.evaluate("window.controlPlaneTour.setSlide(3)")
    page.wait_for_timeout(50)
    page.click('.policy-option[data-code="full_access"]')
    assert "R0–R3 自动批准" in page.text_content("#launch-access")
    page.click("#close-button")
    assert page.locator("#exit-layer").is_visible()
    page.keyboard.press("Escape")
    assert page.locator("#exit-layer").is_hidden()

    page.evaluate("window.controlPlaneTour.setSlide(0)")
    page.wait_for_timeout(700)
    box = page.locator("#slider-viewport").bounding_box()
    assert box is not None
    page.mouse.move(box["x"] + box["width"] * 0.75, box["y"] + box["height"] * 0.55)
    page.mouse.down()
    page.mouse.move(box["x"] + box["width"] * 0.35, box["y"] + box["height"] * 0.55, steps=8)
    page.mouse.up()
    page.wait_for_timeout(650)
    assert page.evaluate("window.controlPlaneTour.current") == 1
    context.close()

    compact = browser.new_context(
        viewport={"width": 1024, "height": 768},
        device_scale_factor=1,
        reduced_motion="reduce",
    )
    compact_page = compact.new_page()
    compact_page.goto(URL, wait_until="load")
    compact_page.wait_for_function(
        "Array.from(document.images).every(image => image.complete && image.naturalWidth > 0)"
    )
    for index in range(4):
        compact_page.evaluate("index => window.controlPlaneTour.setSlide(index)", index)
        assert_inside_viewport(compact_page)
    compact.close()
    browser.close()

if errors:
    raise SystemExit("\n".join(errors))

print("Rendered 4 screens and passed interaction + 1024x768 layout checks.")
