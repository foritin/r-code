from pathlib import Path

from playwright.sync_api import sync_playwright


ROOT = Path(__file__).resolve().parent
URL = (ROOT / "index.html").as_uri()
NAMES = [
    "onboarding-01-welcome.png",
    "onboarding-02-provider.png",
    "onboarding-03-credentials.png",
    "onboarding-04-first-task.png",
]


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
    page.on("console", lambda message: errors.append(f"console {message.type}: {message.text}") if message.type == "error" else None)
    page.on("pageerror", lambda error: errors.append(f"pageerror: {error}"))
    page.goto(URL, wait_until="load")
    page.evaluate("document.fonts.ready")
    page.wait_for_function("Array.from(document.images).every(image => image.complete && image.naturalWidth > 0)")
    page.wait_for_timeout(500)
    for index, name in enumerate(NAMES):
        page.evaluate("index => window.onboardingDeck.setSlide(index)", index)
        page.wait_for_timeout(350)
        page.screenshot(path=str(ROOT / name), full_page=False)
    browser.close()

if errors:
    raise SystemExit("\n".join(errors))
