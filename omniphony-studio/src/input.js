import * as THREE from 'three';

export function setupNumericWheelEditing() {
  const decimalsFromStep = (stepAttr) => {
    if (!stepAttr || stepAttr === 'any') return null;
    const step = Number(stepAttr);
    if (!Number.isFinite(step) || step <= 0) return null;
    const raw = String(stepAttr).toLowerCase();
    if (raw.includes('e')) {
      const [mantissa, expRaw] = raw.split('e');
      const exp = Number(expRaw);
      if (!Number.isFinite(exp)) return null;
      const fracLen = (mantissa.split('.')[1] || '').length;
      return Math.max(0, fracLen - exp);
    }
    return (raw.split('.')[1] || '').length;
  };

  const numberInputs = Array.from(document.querySelectorAll('input[type="number"]'));
  numberInputs.forEach((inputEl) => {
    inputEl.addEventListener('wheel', (event) => {
      if (inputEl.disabled || inputEl.readOnly) return;
      const delta = Math.sign(event.deltaY);
      if (delta === 0) return;

      event.preventDefault();
      event.stopPropagation();
      if (document.activeElement !== inputEl) {
        inputEl.focus({ preventScroll: true });
      }

      const before = inputEl.value;
      const repeats = event.shiftKey ? 10 : 1;
      try {
        for (let i = 0; i < repeats; i += 1) {
          if (delta < 0) inputEl.stepUp();
          else inputEl.stepDown();
        }
      } catch (_e) {
        return;
      }
      if (inputEl.value === before) return;
      const decimals = decimalsFromStep(inputEl.getAttribute('step'));
      if (decimals !== null) {
        const v = Number(inputEl.value);
        if (Number.isFinite(v)) {
          inputEl.value = v.toFixed(decimals);
        }
      }
      inputEl.dispatchEvent(new Event('input', { bubbles: true }));
    }, { passive: false });
  });
}

export function projectRayOntoAxis(rayOrigin, rayDirection, axisOrigin, axisDirection) {
  const w0 = new THREE.Vector3().subVectors(axisOrigin, rayOrigin);
  const b = axisDirection.dot(rayDirection);
  const d = axisDirection.dot(w0);
  const e = rayDirection.dot(w0);
  const den = 1 - (b * b);
  if (Math.abs(den) < 1e-6) {
    return 0;
  }
  return ((b * e) - d) / den;
}
