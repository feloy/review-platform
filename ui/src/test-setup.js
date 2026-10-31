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

// Material Web's ripple uses the Web Animations API, which jsdom lacks.
if (!window.Element.prototype.animate) {
  window.Element.prototype.animate = () => ({ currentTime: 1000, cancel() {} });
}
