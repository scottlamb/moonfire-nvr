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

import 'jquery-ui/themes/base/button.css';
import 'jquery-ui/themes/base/core.css';
import 'jquery-ui/themes/base/dialog.css';
import 'jquery-ui/themes/base/theme.css';
// This not needed for pure dialog, but we want it resizable
import 'jquery-ui/themes/base/resizable.css';

// Get dialog ui widget
import 'jquery-ui/ui/widgets/dialog';

/**
 * Class to implement a simple jQuery dialog based video player.
 */
export default class VideoDialogView {
  /**
   * Construct the player.
   *
   * This does not attach the player to the DOM anywhere! In fact, construction
   * of the necessary video element is delayed until an attach is requested.
   * Since the close of the video removes all traces of it in the DOM, this
   * apprach allows repeated use by calling attach again!
   */
  constructor() {}

  /**
   * Attach the player to the specified DOM element.
   *
   * @param {Node} domElement DOM element to attach to
   * @return {VideoDialogView} Returns "this" for chaining.
   */
  attach(domElement) {
    this._videoElement = $('<video controls preload="auto" autoplay="true" />');
    this._dialogElement = $('<div class="playdialog" />').append(
        this._videoElement
    );
    $(domElement).append(this._dialogElement);
    return this;
  }

  /**
   * Show the player, and start playing the given url.
   *
   * @param  {String} title Title of the video player
   * @param  {Number} width Width of the player
   * @param  {String} url   URL for source media
   * @return {VideoDialogView}       Returns "this" for chaining.
   */
  play(title, width, url) {
    this._dialogElement.dialog({
      title: title,
      width: width,
      close: () => {
        const videoDOMElement = this._videoElement[0];
        videoDOMElement.pause();
        videoDOMElement.src = ''; // Remove current source to stop loading
        this._videoElement = null;
        this._dialogElement.remove();
        this._dialogElement = null;
      },
    });
    // Now that dialog is up, set the src so video starts
    console.log('Video url: ' + url);
    this._videoElement.attr('src', url);
    return this;
  }
}
