(function () {
  try {
    var tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
    document.cookie = "tz=" + tz + "; Path=/; SameSite=Lax; Max-Age=31536000";
    var el = document.querySelector('input[type="hidden"][name="timezone"]');
    if (el) el.value = tz;
  } catch (e) {}
  var setNow = function () {
    var d = new Date();
    var v = d.getHours() * 60 + d.getMinutes();
    document.querySelectorAll("[data-now-line]").forEach(function (t) {
      t.style.setProperty("--now", v);
    });
  };
  if (document.querySelector("[data-now-line]")) {
    setNow();
    setInterval(setNow, 60000);
  }
  document.querySelectorAll("[data-scroll-now]").forEach(function (el) {
    var line = el.querySelector(".now-line");
    if (!line) return;
    var top = line.getBoundingClientRect().top;
    var p = el.parentElement;
    while (p && p !== document.body && p.scrollHeight <= p.clientHeight + 1) p = p.parentElement;
    if (p && p !== document.body) {
      p.scrollTop += top - p.getBoundingClientRect().top - p.clientHeight / 3;
    } else {
      scrollTo(0, scrollY + top - innerHeight / 3);
    }
  });
  document.addEventListener("click", function (ev) {
    var t = ev.target;
    if (!t.closest) return;
    var closer = t.closest("[data-toast-close]");
    if (closer) {
      var toast = closer.closest(".toast");
      if (toast) toast.remove();
      return;
    }
    var btn = t.closest("button[data-preset]");
    var form = btn && btn.closest("form");
    if (!form) return;
    var names = "monday tuesday wednesday thursday friday saturday sunday".split(" ");
    var masks = { weekdays: 31, weekends: 96, all: 127 };
    var mask = masks[btn.dataset.preset] || 0;
    names.forEach(function (n, i) {
      var box = form.elements[n];
      if (box) box.checked = (mask >> i & 1) === 1;
    });
  });
})();
