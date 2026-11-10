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
    let timer: ReturnType<typeof setTimeout>;
    const animation = Object.assign(new EventTarget(), {
      currentTime: 1000,
      cancel: () => clearTimeout(timer),
    }) as Animation;
    timer = setTimeout(() => animation.dispatchEvent(new Event('finish')), 0);
    return animation;
  };
}
