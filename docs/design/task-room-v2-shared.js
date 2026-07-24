const byId = (id) => document.getElementById(id);

document.querySelectorAll('[data-toggle]').forEach((button) => {
  button.addEventListener('click', () => {
    const target = byId(button.dataset.toggle);
    if (!target) return;
    const willOpen = target.hidden;
    target.hidden = !willOpen;
    button.setAttribute('aria-expanded', String(willOpen));
    if (button.dataset.openLabel && button.dataset.closedLabel) {
      const label = button.querySelector('[data-toggle-label]');
      if (label) {
        label.textContent = willOpen ? button.dataset.openLabel : button.dataset.closedLabel;
      }
    }
  });
});

document.querySelectorAll('[data-show]').forEach((button) => {
  button.addEventListener('click', () => {
    const target = byId(button.dataset.show);
    if (!target) return;
    target.hidden = false;
    target.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
  });
});

document.querySelectorAll('[data-activate-tab]').forEach((button) => {
  button.addEventListener('click', () => {
    const tab = document.querySelector(`[data-tab="${button.dataset.activateTab}"]`);
    if (!(tab instanceof HTMLElement)) return;
    tab.click();
    tab.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
  });
});

document.querySelectorAll('[data-focus]').forEach((button) => {
  button.addEventListener('click', () => {
    const target = byId(button.dataset.focus);
    if (!target) return;
    target.focus();
    target.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
  });
});

document.querySelectorAll('[data-accept]').forEach((button) => {
  button.addEventListener('click', () => {
    document.body.dataset.reviewState = 'accepted';
    document.querySelectorAll('[data-status-text]').forEach((node) => {
      node.textContent = '已接受';
    });
    document.querySelectorAll('[data-accept]').forEach((acceptButton) => {
      acceptButton.textContent = '改动已接受';
      acceptButton.disabled = true;
    });
    document.querySelectorAll('[data-after-accept]').forEach((node) => {
      node.hidden = false;
    });
  });
});

document.querySelectorAll('[data-tab]').forEach((button) => {
  button.addEventListener('click', () => {
    const group = button.closest('[data-tabs]');
    if (!group) return;
    group.querySelectorAll('[data-tab]').forEach((candidate) => {
      const selected = candidate === button;
      candidate.classList.toggle('active', selected);
      candidate.setAttribute('aria-selected', String(selected));
    });
    const scope = group.parentElement;
    scope?.querySelectorAll('[data-panel]').forEach((panel) => {
      panel.hidden = panel.dataset.panel !== button.dataset.tab;
    });
  });
});
