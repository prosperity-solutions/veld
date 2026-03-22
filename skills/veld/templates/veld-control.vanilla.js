/**
 * Veld Controls — Plain JavaScript binding
 *
 * Drop this into your page script. No framework, no dependencies.
 * Reads from window.__veld_controls injected by veld.
 *
 * Usage:
 *   // Bind a value to a DOM element
 *   veldControl("duration", 200, function(val) {
 *     document.querySelector(".animated").style.transitionDuration = val + "ms";
 *   });
 *
 *   // Bind a color
 *   veldControl("accent", "#3b82f6", function(val) {
 *     document.documentElement.style.setProperty("--accent", val);
 *   });
 *
 *   // Bind an action button
 *   veldAction("retry", function() {
 *     location.reload();
 *   });
 *
 * The agent will add/remove these bindings automatically.
 */

/**
 * Bind a callback to a veld control value.
 * Fires immediately with current value, then on every change.
 *
 * @param {string} name — control name
 * @param {*} defaultValue — fallback when veld is not running
 * @param {function} callback — called with (newValue)
 * @returns {function} unsubscribe — call to stop listening
 */
function veldControl(name, defaultValue, callback) {
  var controls = window.__veld_controls;
  if (!controls) {
    callback(defaultValue);
    return function() {};
  }
  var current = controls.get(name);
  callback(current !== undefined ? current : defaultValue);
  return controls.on(name, callback);
}

/**
 * Bind a callback to a veld action button (retry, start, stop, etc.)
 *
 * @param {string} name — action name
 * @param {function} callback — fires when the user clicks the button
 * @returns {function} unsubscribe
 */
function veldAction(name, callback) {
  var controls = window.__veld_controls;
  if (!controls) return function() {};
  return controls.onAction(name, callback);
}

/**
 * Shortcut: bind a veld control directly to a CSS custom property.
 * No callback needed — updates the property on :root automatically.
 *
 * @param {string} name — control name
 * @param {string} cssVar — CSS custom property name (e.g. "--duration")
 * @param {*} defaultValue — fallback
 * @param {string} [unit] — optional unit suffix (e.g. "ms", "px")
 * @returns {function} unsubscribe
 */
function veldCSSVar(name, cssVar, defaultValue, unit) {
  return veldControl(name, defaultValue, function(val) {
    document.documentElement.style.setProperty(cssVar, val + (unit || ""));
  });
}
