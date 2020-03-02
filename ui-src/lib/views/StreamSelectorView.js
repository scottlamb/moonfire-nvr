// vim: set et sw=2 ts=2:
//
// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

import $ from 'jquery';

const allStreamTypes = ['main', 'sub'];

/**
 * View for selecting the enabled streams.
 *
 * This displays a table with a camera per row and stream type per column.
 * It propagates the enabled status on to the stream view. It also calls
 * the optional onChange handler on any change.
 */
export default class StreamSelectorView {
  /**
   * @param {Array} cameras An element for each camera with
   *                        - camera: a {Camera}
   *                        - streamViews: a map of stream type to {StreamView}
   * @param {jQuery} parent jQuery parent element to append to
   */
  constructor(cameras, parent) {
    this._cameras = cameras;

    if (cameras.length !== 0) {
      // Add a header row.
      const hdr = $('<tr/>').append($('<th/>'));
      for (const streamType of allStreamTypes) {
        hdr.append($('<th/>').text(streamType));
      }
      parent.append(hdr);
    }

    this._cameras.forEach((c) => {
      const row = $('<tr/>').append($('<td>').text(c.camera.shortName));
      let firstStreamType = true;
      for (const streamType of allStreamTypes) {
        const streamView = c.streamViews[streamType];
        if (streamView === undefined) {
          row.append('<td/>');
        } else {
          const id = 'cam-' + c.camera.uuid + '-' + streamType;
          const cb = $('<input type="checkbox">')
              .attr('name', id)
              .attr('id', id);

          // Only the first stream type for each camera should be checked
          // initially.
          cb.prop('checked', firstStreamType);
          streamView.enabled = firstStreamType;
          firstStreamType = false;

          cb.change((e) => {
            streamView.enabled = e.target.checked;
            if (this._onChangeHandler) {
              this._onChangeHandler();
            }
          });
          row.append($('<td/>').append(cb));
        }
      }
      parent.append(row);
    });

    this._onChangeHandler = null;
  }

  /** @param {function()} handler a handler to run after toggling a stream */
  set onChange(handler) {
    this._onChangeHandler = handler;
  }
}
