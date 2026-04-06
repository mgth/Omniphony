function byId(root, id) {
  if (!root) return null;
  return root.querySelector(`#${id}`);
}

function queryAll(root, selector) {
  if (!root) return [];
  return Array.from(root.querySelectorAll(selector));
}

function panelRoot(id) {
  return document.getElementById(id);
}

export function getInputPanelRoot() {
  return panelRoot('inputPanelRoot');
}

export function getAudioPanelRoot() {
  return panelRoot('audioPanelRoot');
}

export function getRendererPanelRoot() {
  return panelRoot('rendererPanelRoot');
}

export function getRoomGeometryPanelRoot() {
  return panelRoot('roomGeometryPanelRoot');
}

export function getDisplayPanelRoot() {
  return panelRoot('displayPanelRoot');
}

export function getOscPanelRoot() {
  return panelRoot('oscPanelRoot');
}

export function getObjectsPanelRoot() {
  return panelRoot('objectsPanelRoot');
}

export function getSaveFooterRoot() {
  return panelRoot('saveFooterRoot');
}

export function getRendererInfoModalsRoot() {
  return panelRoot('rendererInfoModalsRoot');
}

export function inInputPanel(id) {
  return byId(getInputPanelRoot(), id);
}

export function inAudioPanel(id) {
  return byId(getAudioPanelRoot(), id);
}

export function inRendererPanel(id) {
  return byId(getRendererPanelRoot(), id);
}

export function inRoomGeometryPanel(id) {
  return byId(getRoomGeometryPanelRoot(), id);
}

export function inDisplayPanel(id) {
  return byId(getDisplayPanelRoot(), id);
}

export function inOscPanel(id) {
  return byId(getOscPanelRoot(), id);
}

export function inObjectsPanel(id) {
  return byId(getObjectsPanelRoot(), id);
}

export function inSaveFooter(id) {
  return byId(getSaveFooterRoot(), id);
}

export function inRendererInfoModals(id) {
  return byId(getRendererInfoModalsRoot(), id);
}

export function inputPanelQueryAll(selector) {
  return queryAll(getInputPanelRoot(), selector);
}

export function roomGeometryPanelQueryAll(selector) {
  return queryAll(getRoomGeometryPanelRoot(), selector);
}
