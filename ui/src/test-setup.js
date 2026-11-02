// Material Web uses matchMedia, which jsdom does not implement.
window.matchMedia = (media) => ({
  media,
  matches: false,
  onchange: null,
  addListener() {},
  removeListener() {},
  addEventListener() {},
  removeEventListener() {},
  dispatchEvent() { return true; },
});

// jsdom's ElementInternals does not yet support form values.
if (!window.ElementInternals.prototype.setFormValue) {
  window.ElementInternals.prototype.setFormValue = () => {};
}

// jsdom has no top layer; track tooltip opening and dismissal without simulating layout.
if (!window.HTMLElement.prototype.showPopover) {
  window.HTMLElement.prototype.showPopover = function () { this.setAttribute('data-test-popover-open', ''); };
  window.HTMLElement.prototype.hidePopover = function () { this.removeAttribute('data-test-popover-open'); };
}

// Material Web ripples and menus use Web Animations, which jsdom lacks.
if (!window.Element.prototype.animate) {
  window.Element.prototype.animate = () => {
    const animation = new EventTarget();
    animation.currentTime = 1000;
    const timer = setTimeout(() => animation.dispatchEvent(new Event('finish')), 0);
    animation.cancel = () => clearTimeout(timer);
    return animation;
  };
}
