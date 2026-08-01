from pathlib import Path

from playwright.sync_api import sync_playwright


ROOT = Path(__file__).resolve().parent
URL = (ROOT / "index.html").as_uri()
NAMES = [
    "focus-01-welcome.png",
    "focus-02-agent.png",
    "focus-03-provider.png",
    "focus-04-workspace.png",
    "focus-05-ready.png",
]


def assert_inside_viewport(page) -> None:
    result = page.evaluate(
        """
        () => {
          const rect = document.querySelector('.tour').getBoundingClientRect();
          return {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
            width: innerWidth,
            height: innerHeight,
            overflowX: document.documentElement.scrollWidth > innerWidth
          };
        }
        """
    )
    assert result["left"] >= -1 and result["top"] >= -1, result
    assert result["right"] <= result["width"] + 1, result
    assert result["bottom"] <= result["height"] + 1, result
    assert not result["overflowX"], result


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
        if index == 4:
            page.evaluate("window.focusTour.setSlide(2)")
            page.wait_for_timeout(600)
            page.fill("#api-key", "prototype-only-key")
            page.click("#save-key")
        page.evaluate("index => window.focusTour.setSlide(index)", index)
        page.wait_for_timeout(600)
        assert_inside_viewport(page)
        page.screenshot(path=str(ROOT / name), full_page=False)

    # Provider state + validation + mock success.
    page.evaluate("window.focusTour.setSlide(2)")
    page.wait_for_timeout(600)
    page.click('.provider-tab[data-provider="OpenAI"]')
    assert page.text_content("#provider-model") == "gpt-5.6-sol"
    assert page.text_content("#provider-protocol-code") == "openai_responses"
    page.click("#save-key")
    assert page.locator("#key-field").evaluate("node => node.classList.contains('error')")
    page.fill("#api-key", "prototype-only-key")
    page.click("#save-key")
    assert page.locator("#key-field").evaluate("node => node.classList.contains('saved')")
    assert page.input_value("#api-key") == ""

    # Workspace and permission state.
    page.evaluate("window.focusTour.setSlide(3)")
    page.wait_for_timeout(600)
    page.click('.scope-toggle button[data-scope="chat"]')
    assert "纯聊天" in page.text_content("#summary-workspace")
    assert page.text_content("#summary-access") == "不适用"
    page.click('.scope-toggle button[data-scope="workspace"]')
    page.click('.access-tabs button[data-mode="完全访问权限"]')
    assert "R0–R3 自动" in page.text_content("#summary-access")

    # Close and Escape recovery.
    page.click("#close-button")
    assert page.locator("#dialog-layer").is_visible()
    page.keyboard.press("Escape")
    assert page.locator("#dialog-layer").is_hidden()

    # Horizontal drag.
    page.evaluate("window.focusTour.setSlide(0)")
    page.wait_for_timeout(650)
    box = page.locator("#slider-viewport").bounding_box()
    assert box is not None
    page.mouse.move(box["x"] + box["width"] * .72, box["y"] + box["height"] * .55)
    page.mouse.down()
    page.mouse.move(box["x"] + box["width"] * .30, box["y"] + box["height"] * .55, steps=8)
    page.mouse.up()
    page.wait_for_timeout(650)
    assert page.evaluate("window.focusTour.current") == 1

    # The floating panel itself can be moved from its header.
    before = page.locator(".tour").bounding_box()
    header = page.locator(".tour-header").bounding_box()
    assert before is not None and header is not None
    page.mouse.move(header["x"] + header["width"] * .48, header["y"] + header["height"] * .5)
    page.mouse.down()
    page.mouse.move(header["x"] + header["width"] * .48 + 28, header["y"] + header["height"] * .5 + 18, steps=5)
    page.mouse.up()
    after = page.locator(".tour").bounding_box()
    assert after is not None and after["x"] > before["x"] + 20 and after["y"] > before["y"] + 10

    # Complete path after key is marked ready.
    page.evaluate("window.focusTour.markKeySaved(); window.focusTour.setSlide(4)")
    page.wait_for_timeout(650)
    page.click("#create-session")
    assert page.locator("#dialog-layer").is_visible()
    assert page.text_content("#dialog-title") == "会话配置已确认。"
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
    for index in range(5):
        compact_page.evaluate("index => window.focusTour.setSlide(index)", index)
        assert_inside_viewport(compact_page)
    compact.close()
    browser.close()

if errors:
    raise SystemExit("\n".join(errors))

print("Rendered 5 screens and passed flow, validation, drag, close and 1024x768 checks.")
