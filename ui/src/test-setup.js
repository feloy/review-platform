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
