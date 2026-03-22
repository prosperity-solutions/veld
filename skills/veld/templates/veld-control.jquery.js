/**
 * Veld Controls — jQuery binding
 *
 * Drop this into your page script. No npm package needed.
 * Reads from window.__veld_controls injected by veld.
 *
 * Usage:
 *   // Bind a CSS property to a control
 *   $.veldControl("duration", 200, function(val) {
 *     $(".animated").css("transition-duration", val + "ms");
 *   });
 *
 *   // Bind an action button
 *   $.veldAction("retry", function() {
 *     $.ajax("/api/data").then(render);
 *   });
 *
 * The agent will add/remove these bindings automatically.
 */

(function($) {
  /**
   * Bind a callback to a veld control value.
   * Fires immediately with current value, then on every change.
   *
   * @param {string} name — control name
   * @param {*} defaultValue — fallback when veld is not running
   * @param {function} callback — called with new value
   * @returns {function} unsubscribe function
   */
  $.veldControl = function(name, defaultValue, callback) {
    var controls = window.__veld_controls;
    if (!controls) {
      callback(defaultValue);
      return function() {};
    }
    var current = controls.get(name);
    callback(current !== undefined ? current : defaultValue);
    return controls.on(name, callback);
  };

  /**
   * Bind a callback to a veld action button.
   *
   * @param {string} name — action name
   * @param {function} callback — fires on button click
   * @returns {function} unsubscribe function
   */
  $.veldAction = function(name, callback) {
    var controls = window.__veld_controls;
    if (!controls) return function() {};
    return controls.onAction(name, callback);
  };
})(jQuery);
