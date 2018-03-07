// vim: set et sw=2:

import NVRApplication from './NVRApplication';

import $ from 'jquery';

// On document load, start application
$(function() {
  $(document).tooltip();
  (new NVRApplication()).start();
});
