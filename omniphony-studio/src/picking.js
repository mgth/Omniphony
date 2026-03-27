import * as THREE from 'three';
import { app, speakerMeshes, sourceMeshes, sourceLabels, sourceOutlines, speakerLabels } from './state.js';
import { scene, camera, renderer, controls } from './scene/setup.js';
import { raycaster, pointer } from './scene/materials.js';
import { speakerGizmo, distanceGizmo, cartesianGizmo } from './scene/gizmos.js';
import { normalizeAngleDeg, snapAngleDeg, sphericalToCartesianDeg, normalizedOmniphonyToScenePosition } from './coordinates.js';
import { applySpeakerPolarEdit, applySpeakerCartesianEdit, applySpeakerSceneCartesianEdit, setSelectedSpeaker, renderSpeakerEditor } from './speakers.js';
import { setSelectedSource } from './sources.js';
import { projectRayOntoAxis } from './input.js';

let boundCanvas = null;

function onPointerDown(event) {
  app.pointerDownPosition = { x: event.clientX, y: event.clientY };
  if (beginSpeakerDrag(event)) {
    app.pointerDownPosition = null;
  }
}

function onPointerUp(event) {
  if (app.isDraggingSpeaker && event.pointerId === app.draggingPointerId) {
    endSpeakerDrag();
  }
  if (!app.pointerDownPosition) {
    return;
  }

  const dx = event.clientX - app.pointerDownPosition.x;
  const dy = event.clientY - app.pointerDownPosition.y;
  app.pointerDownPosition = null;

  if (Math.hypot(dx, dy) <= 6) {
    const hitSceneItem = selectSceneItemFromPointer(event);
    if (hitSceneItem) {
      return;
    }
    setSelectedSource(null);
    setSelectedSpeaker(null);
    updateControlsForEditMode();
  }
}

function onPointerMove(event) {
  if (app.isDraggingSpeaker && event.pointerId === app.draggingPointerId) {
    updateSpeakerDrag(event);
  }
}

function onPointerCancel() {
  endSpeakerDrag();
}

function onPointerLeave() {
  endSpeakerDrag();
}

function onWheel(event) {
  if (app.activeEditMode !== 'polar' || app.selectedSpeakerIndex === null || !app.polarEditArmed) {
    return;
  }
  if (!event.ctrlKey && !event.shiftKey) {
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  const prevZoom = controls.enableZoom;
  controls.enableZoom = false;

  const delta = -Math.sign(event.deltaY);
  const step = event.shiftKey ? 0.01 : 0.05;
  const next = Math.min(2.0, Math.max(0.2, app.dragDistance + delta * step));
  if (next === app.dragDistance) {
    return;
  }
  app.dragDistance = next;
  const pos = sphericalToCartesianDeg(app.dragAzimuthDeg, app.dragElevationDeg, app.dragDistance);
  const mesh = speakerMeshes[app.selectedSpeakerIndex];
  if (mesh) {
    mesh.position.set(pos.x, pos.y, pos.z);
  }
  const label = speakerLabels[app.selectedSpeakerIndex];
  if (label) {
    label.position.set(pos.x, pos.y + 0.12, pos.z);
  }
  const speaker = app.currentLayoutSpeakers[app.selectedSpeakerIndex];
  if (speaker) {
    applySpeakerSceneCartesianEdit(app.selectedSpeakerIndex, pos.x, pos.y, pos.z, false);
  }
  controls.enableZoom = prevZoom;
}

export function rebindPointerListeners() {
  const canvas = renderer.domElement;
  if (boundCanvas === canvas) {
    return;
  }
  if (boundCanvas) {
    boundCanvas.removeEventListener('pointerdown', onPointerDown);
    boundCanvas.removeEventListener('pointerup', onPointerUp);
    boundCanvas.removeEventListener('pointermove', onPointerMove);
    boundCanvas.removeEventListener('pointercancel', onPointerCancel);
    boundCanvas.removeEventListener('pointerleave', onPointerLeave);
    boundCanvas.removeEventListener('wheel', onWheel, true);
  }
  canvas.addEventListener('pointerdown', onPointerDown);
  canvas.addEventListener('pointerup', onPointerUp);
  canvas.addEventListener('pointermove', onPointerMove);
  canvas.addEventListener('pointercancel', onPointerCancel);
  canvas.addEventListener('pointerleave', onPointerLeave);
  canvas.addEventListener('wheel', onWheel, { passive: false, capture: true });
  boundCanvas = canvas;
}

function updateControlsForEditMode() {
  controls.enableZoom = true;
}

export function pointerEventToNdc(event) {
  const rect = renderer.domElement.getBoundingClientRect();
  pointer.x = ((event.clientX - rect.left) / rect.width) * 2 - 1;
  pointer.y = -((event.clientY - rect.top) / rect.height) * 2 + 1;
}

export function getPickableSceneTargets() {
  const sourceTargets = [
    ...sourceLabels.values(),
    ...sourceMeshes.values(),
    ...sourceOutlines.values()
  ].filter((object) => object && object.visible !== false);
  const speakerTargets = [
    ...speakerMeshes,
    ...speakerLabels
  ].filter((object) => object && object.visible !== false);
  return [...sourceTargets, ...speakerTargets];
}

export function pickSpeakerFromIntersects(intersects) {
  for (const hit of intersects) {
    const object = hit.object;
    const speakerIdx = speakerMeshes.indexOf(object);
    if (speakerIdx >= 0) {
      setSelectedSource(null);
      setSelectedSpeaker(speakerIdx);
      return true;
    }

    const labelIdx = speakerLabels.indexOf(object);
    if (labelIdx >= 0) {
      setSelectedSource(null);
      setSelectedSpeaker(labelIdx);
      return true;
    }
  }
  return false;
}

export function selectSceneItemFromPointer(event) {
  pointerEventToNdc(event);
  raycaster.setFromCamera(pointer, camera);
  const intersects = raycaster.intersectObjects(getPickableSceneTargets(), false);

  if (pickSpeakerFromIntersects(intersects)) {
    return true;
  }

  for (const hit of intersects) {
    const object = hit.object;
    const sourceId = object?.userData?.sourceId;
    if (sourceId !== undefined && sourceId !== null) {
      setSelectedSource(sourceId);
      setSelectedSpeaker(null);
      updateControlsForEditMode();
      return true;
    }

    const speakerIdx = speakerMeshes.indexOf(object);
    if (speakerIdx >= 0) {
      setSelectedSource(null);
      setSelectedSpeaker(speakerIdx);
      return true;
    }

    const labelIdx = speakerLabels.indexOf(object);
    if (labelIdx >= 0) {
      setSelectedSource(null);
      setSelectedSpeaker(labelIdx);
      return true;
    }
  }

  return false;
}

export function beginSpeakerDrag(event) {
  if (app.selectedSpeakerIndex === null) {
    return false;
  }
  pointerEventToNdc(event);
  raycaster.setFromCamera(pointer, camera);

  if (app.activeEditMode === 'polar' && app.polarEditArmed) {
    const gizmoHits = raycaster.intersectObjects([speakerGizmo.ring, speakerGizmo.arc], false);
    if (gizmoHits.length === 0) {
      return false;
    }
    const hit = gizmoHits[0].object;
    app.dragMode = hit === speakerGizmo.ring ? 'azimuth' : 'elevation';
    app.isDraggingSpeaker = true;
    app.draggingPointerId = event.pointerId;
    app.dragAzimuthDelta = 1;
    app.dragElevationDelta = 1;
    controls.enabled = false;
    return true;
  }

  if (app.activeEditMode === 'cartesian' && app.cartesianEditArmed) {
    const handleHits = raycaster.intersectObjects(
      [cartesianGizmo.xHandle, cartesianGizmo.yHandle, cartesianGizmo.zHandle],
      false
    );
    if (handleHits.length === 0) {
      return false;
    }
    const axis = handleHits[0].object?.userData?.axis;
    if (!axis) {
      return false;
    }
    app.dragMode = 'cartesian';
    app.dragAxis = axis;
    app.dragAxisDirection.set(
      axis === 'x' ? 1 : 0,
      axis === 'y' ? 1 : 0,
      axis === 'z' ? 1 : 0
    );
    const mesh = speakerMeshes[app.selectedSpeakerIndex];
    if (mesh) {
      app.dragAxisOrigin.copy(mesh.position);
      app.dragSpeakerStartPosition.copy(mesh.position);
      app.dragAxisStartT = projectRayOntoAxis(
        raycaster.ray.origin,
        raycaster.ray.direction,
        app.dragAxisOrigin,
        app.dragAxisDirection
      );
    }
    app.isDraggingSpeaker = true;
    app.draggingPointerId = event.pointerId;
    controls.enabled = false;
    return true;
  }

  return false;
}

export function updateSpeakerDrag(event) {
  if (!app.isDraggingSpeaker || app.selectedSpeakerIndex === null) {
    return;
  }
  pointerEventToNdc(event);
  raycaster.setFromCamera(pointer, camera);

  if (app.dragMode === 'azimuth') {
    const plane = new THREE.Plane(new THREE.Vector3(0, 1, 0), 0);
    const hitPoint = new THREE.Vector3();
    if (raycaster.ray.intersectPlane(plane, hitPoint)) {
      app.dragAzimuthDeg = (Math.atan2(hitPoint.z, hitPoint.x) * 180) / Math.PI;
      app.dragAzimuthDeg = normalizeAngleDeg(app.dragAzimuthDeg);
      const radial = Math.sqrt(hitPoint.x * hitPoint.x + hitPoint.z * hitPoint.z);
      const delta = (radial - app.dragDistance) / app.dragDistance;
      app.dragAzimuthDelta = delta;
      if (delta >= 0 && delta <= 0.1) {
        app.dragAzimuthDeg = snapAngleDeg(app.dragAzimuthDeg, 1, 0.5);
      } else if (delta > 0.1) {
        app.dragAzimuthDeg = snapAngleDeg(app.dragAzimuthDeg, 5, 2.5);
      }
    }
  } else if (app.dragMode === 'elevation') {
    const azRad = (app.dragAzimuthDeg * Math.PI) / 180;
    const dir = new THREE.Vector3(Math.cos(azRad), 0, Math.sin(azRad));
    const normal = new THREE.Vector3().crossVectors(dir, new THREE.Vector3(0, 1, 0)).normalize();
    const plane = new THREE.Plane().setFromNormalAndCoplanarPoint(normal, new THREE.Vector3(0, 0, 0));
    const hitPoint = new THREE.Vector3();
    if (raycaster.ray.intersectPlane(plane, hitPoint)) {
      const planar = Math.sqrt(hitPoint.x * hitPoint.x + hitPoint.z * hitPoint.z);
      app.dragElevationDeg = (Math.atan2(hitPoint.y, planar) * 180) / Math.PI;
      app.dragElevationDeg = Math.max(-90, Math.min(90, app.dragElevationDeg));
      const radius = Math.sqrt(hitPoint.x * hitPoint.x + hitPoint.y * hitPoint.y + hitPoint.z * hitPoint.z);
      const delta = (radius - app.dragDistance) / app.dragDistance;
      app.dragElevationDelta = delta;
      if (delta >= 0 && delta <= 0.1) {
        app.dragElevationDeg = snapAngleDeg(app.dragElevationDeg, 1, 0.5);
      } else if (delta > 0.1) {
        app.dragElevationDeg = snapAngleDeg(app.dragElevationDeg, 5, 2.5);
      }
    }
  } else if (app.dragMode === 'cartesian') {
    const tNow = projectRayOntoAxis(
      raycaster.ray.origin,
      raycaster.ray.direction,
      app.dragAxisOrigin,
      app.dragAxisDirection
    );
    const delta = tNow - app.dragAxisStartT;
    const pos = app.dragSpeakerStartPosition.clone().add(app.dragAxisDirection.clone().multiplyScalar(delta));
    applySpeakerSceneCartesianEdit(app.selectedSpeakerIndex, pos.x, pos.y, pos.z, false);
    return;
  }

  const pos = sphericalToCartesianDeg(app.dragAzimuthDeg, app.dragElevationDeg, app.dragDistance);
  const mesh = speakerMeshes[app.selectedSpeakerIndex];
  if (mesh) {
    mesh.position.set(pos.x, pos.y, pos.z);
  }
  const label = speakerLabels[app.selectedSpeakerIndex];
  if (label) {
    label.position.set(pos.x, pos.y + 0.12, pos.z);
  }
  const speaker = app.currentLayoutSpeakers[app.selectedSpeakerIndex];
  if (speaker) {
    applySpeakerSceneCartesianEdit(app.selectedSpeakerIndex, pos.x, pos.y, pos.z, false);
  }
}

export function endSpeakerDrag() {
  if (!app.isDraggingSpeaker || app.selectedSpeakerIndex === null) {
    return;
  }
  app.isDraggingSpeaker = false;
  app.dragMode = null;
  app.dragAxis = null;
  app.draggingPointerId = null;
  controls.enabled = true;

  if (app.selectedSpeakerIndex !== null) {
    const idx = app.selectedSpeakerIndex;
    const speaker = app.currentLayoutSpeakers[idx];
    if (speaker) {
      const scenePosition = normalizedOmniphonyToScenePosition(speaker);
      applySpeakerSceneCartesianEdit(idx, scenePosition.x, scenePosition.y, scenePosition.z, true);
    }
  }
}

export function setupPointerListeners() {
  rebindPointerListeners();
}
