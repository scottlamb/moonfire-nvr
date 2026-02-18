// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2022 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

/**
 * @fileoverview Selects a datetime range to view, in the NVR's timezone
 *
 * Renders a pair of date pickers for the date range and a radio button
 * for single-day or multi-day selection (disabling or enabling the end date
 * picker, respectively). These date pickers show which dates actually have
 * video for the selected days and only allow selecting those days. As the
 * selected video streams change, the allowed dates change, and the selected
 * date range may automatically tighten.
 *
 * The start and end time pickers are simpler: they simply honor what was
 * selected in the UI.
 *
 * The internal state is all held in one `DaysState` object; `daysStateReducer`
 * updates it consistently for a given operation.
 *
 * Calls `setRange90k` with the final result. Note that not all of
 * `TimerangeSelector`'s internal state changes will actually produce a new
 * `range90k`, e.g.:
 *
 * - clicking "To other day" (multi-day selection) doesn't by itself
 *  change the result; it just allows subsequent UI clicks to do so.
 * - selecting another stream may expand the list of possible days but doesn't
 *   also by itself doesn't change the time range.
 *
 * # Limitations
 *
 * This has several known problems with time zone handling, including:
 *
 * - doesn't correctly handle times that exist for the NVR's timezone but not in
 *   the browser's. Specifically, consider the case in which the browser's
 *   timezone changes for daylight saving but the NVR doesn't. A Javascript
 *   `Date` object simply can't represent times during the "spring forward"
 *   hour. We are currently using `date-fn`, which has the fundamental design
 *   flaw of assuming that all dates (even in a remote timezone) can be
 *   represented by `Date`.
 * - doesn't allow disambiguating times during the "fall back" hour. Ideally
 *   we'd have support not only in the datetime library but also in the UI
 *   time picker component, and it doesn't exist today.
 * - looks up the NVR's time zone name in the browser's time zone database,
 *   rather than actually transferring and using the NVR's time zone definition.
 *   Thus if say one has been updated for a new daylight saving transition date
 *   but the other doesn't, results will be weird.
 *
 * We hope to address these problems after the Javascript Temporal library is
 * standardized.
 */

import { Stream } from "../types";
import {
  StaticDatePicker,
  StaticDatePickerProps,
} from "@mui/x-date-pickers/StaticDatePicker";
import React, { useEffect } from "react";
import { zonedTimeToUtc } from "date-fns-tz";
import { addDays, addMilliseconds, differenceInMilliseconds } from "date-fns";
import startOfDay from "date-fns/startOfDay";
import FormControlLabel from "@mui/material/FormControlLabel";
import FormLabel from "@mui/material/FormLabel";
import Radio from "@mui/material/Radio";
import RadioGroup from "@mui/material/RadioGroup";
import { TimePicker, TimePickerProps } from "@mui/x-date-pickers/TimePicker";
import Collapse from "@mui/material/Collapse";
import Box from "@mui/material/Box";
import Paper from "@mui/material/Paper";
import { useTheme } from "@mui/material/styles";

interface Props {
  selectedStreams: Set<Stream>;
  timeZoneName: string;
  setRange90k: (range: [number, number] | null) => void;
}

const MyTimePicker = (
  props: Pick<TimePickerProps<Date>, "value" | "onChange" | "disabled">,
) => (
  <TimePicker
    label="Time"
    views={["hours", "minutes", "seconds"]}
    slotProps={{
      textField: {
        fullWidth: true,
        size: "small",
        variant: "outlined",
      },
    }}
    ampm={false}
    {...props}
  />
);

const SmallStaticDatePicker = (props: StaticDatePickerProps<Date>) => {
  // The spacing defined at https://material.io/components/date-pickers#specs
  // seems plenty big enough (on desktop). Not sure why material-ui wants
  // to make it bigger but that doesn't work well with our layout.
  // This adjustment is a fragile hack but seems to work for now.
  // See: https://github.com/mui-org/material-ui/issues/27700
  const DATE_SIZE = 32;
  return (
    <Box
      sx={{
        "@media (pointer: fine)": {
          "& .MuiPickersLayout-root": {
            minWidth: "auto", // defaults to 320px
          },
          "& .MuiPickersLayout-root, & .MuiPickersLayout-contentWrapper, & .MuiDateCalendar-root":
            {
              width: 256, // defaults to 320px
              margin: 0,
            },
          "& .MuiPickersArrowSwitcher-spacer": {
            // By default, this spacer is so big that there's not enough space
            // in the row for October. Shrink it.
            width: 12,
          },
          "& .MuiDayCalendar-weekDayLabel": {
            width: DATE_SIZE,
            margin: 0,
          },
          "& .MuiDayCalendar-slideTransition": {
            minHeight: DATE_SIZE * 6,
          },
          "& .MuiDateCalendar-root": {
            height: "auto",
          },
          "& .MuiDayCalendar-weekContainer": {
            margin: 0,
          },
          "& .MuiPickersDay-dayWithMargin": {
            margin: 0,
          },
          "& .MuiPickersDay-root": {
            width: DATE_SIZE,
            height: DATE_SIZE,
          },
        },
      }}
    >
      <StaticDatePicker {...props} sx={{ background: "transparent" }} />
    </Box>
  );
};

/**
 * Combines the date-part of <tt>dayMillis</tt> and the time part of
 * <tt>time</tt>. If <tt>time</tt> is null, assume it reaches the end of the
 * day.
 */
const combine = (dayMillis: number, time: Date | null) => {
  const start = new Date(dayMillis);
  if (time === null) {
    return addDays(start, 1);
  }
  return addMilliseconds(
    start,
    differenceInMilliseconds(time, startOfDay(time)),
  );
};

/**
 * Allowed days to select (ones with video).
 *
 * These are stored in a funny format: number of milliseconds since epoch of
 * the start of the given day in the browser's time zone. This is because
 *
 * 1. `Date` objects are always in the browser's time zone and `date-fn` rolls
 *     with that, and
 * 2. `Date` objects don't work well in a `Set`. ECMAScript's [equality
 *    rules](https://262.ecma-international.org/7.0/#sec-abstract-equality-comparison)
 *    mean that two different `Date` objects never compare the same.
 */
type AllowedDays = {
  minMillis: number;
  maxMillis: number;
  allMillis: Set<number>;
};

type EndDayType = "same-day" | "other-day";

type DaysState = {
  allowed: AllowedDays | null;

  /**
   * `[start, end]` in same (funny) format as described for `AllowedDays`.
   *
   * This gets mirrored into `range90k` in its expected format (90k units
   * since epoch).
   */
  rangeMillis: [number, number] | null;

  endType: EndDayType;
};

type DaysOpUpdateSelectedStreams = {
  op: "update-selected-streams";
  selectedStreams: Set<Stream>;
};

type DaysOpSetStartDay = {
  op: "set-start-day";
  newStartDate: Date | null;
};

type DaysOpSetEndDay = {
  op: "set-end-day";
  newEndDate: Date;
};

type DaysOpSetEndDayType = {
  op: "set-end-type";
  newEndType: EndDayType;
};

type DaysOp =
  | DaysOpUpdateSelectedStreams
  | DaysOpSetStartDay
  | DaysOpSetEndDay
  | DaysOpSetEndDayType;

/**
 * Computes an <tt>AllowedDays</tt> from the given streams.
 * Returns null if there are no allowed days.
 */
function computeAllowedDayInfo(
  selectedStreams: Set<Stream>,
): AllowedDays | null {
  let minMillis = null;
  let maxMillis = null;
  let allMillis = new Set<number>();
  for (const s of selectedStreams) {
    for (const d in s.days) {
      const t = new Date(d + "T00:00:00").getTime();
      if (minMillis === null || t < minMillis) {
        minMillis = t;
      }
      if (maxMillis === null || t > maxMillis) {
        maxMillis = t;
      }
      allMillis.add(t);
    }
  }
  if (minMillis === null || maxMillis === null) {
    return null;
  }
  return {
    minMillis,
    maxMillis,
    allMillis,
  };
}

const toMillis = (d: Date) => startOfDay(d).getTime();

function daysStateReducer(old: DaysState, op: DaysOp): DaysState {
  let state = { ...old };

  function updateStart(newStart: number) {
    if (
      state.rangeMillis === null ||
      state.endType === "same-day" ||
      state.rangeMillis[1] < newStart
    ) {
      state.rangeMillis = [newStart, newStart];
    } else {
      state.rangeMillis[0] = newStart;
    }
  }

  switch (op.op) {
    case "update-selected-streams":
      state.allowed = computeAllowedDayInfo(op.selectedStreams);
      if (state.allowed === null) {
        state.rangeMillis = null;
      } else if (state.rangeMillis === null) {
        state.rangeMillis = [state.allowed.maxMillis, state.allowed.maxMillis];
      } else {
        if (state.rangeMillis[0] < state.allowed.minMillis) {
          updateStart(state.allowed.minMillis);
        }
        if (state.rangeMillis[1] > state.allowed.maxMillis) {
          state.rangeMillis[1] = state.allowed.maxMillis;
        }
      }
      break;
    case "set-start-day":
      if (op.newStartDate === null) {
        state.rangeMillis = null;
      } else {
        const millis = toMillis(op.newStartDate);
        if (state.allowed === null || state.allowed.minMillis > millis) {
          console.error("Invalid start day selection ", op.newStartDate);
        } else {
          updateStart(millis);
        }
      }
      break;
    case "set-end-day": {
      const millis = toMillis(op.newEndDate);
      if (
        state.rangeMillis === null ||
        state.allowed === null ||
        state.allowed.maxMillis < millis
      ) {
        console.error("Invalid end day selection ", op.newEndDate);
      } else {
        state.rangeMillis[1] = millis;
      }
      break;
    }
    case "set-end-type":
      state.endType = op.newEndType;
      if (state.endType === "same-day" && state.rangeMillis !== null) {
        state.rangeMillis[1] = state.rangeMillis[0];
      }
      break;
  }
  return state;
}

const TimerangeSelector = ({
  selectedStreams,
  timeZoneName,
  setRange90k,
}: Props) => {
  const theme = useTheme();
  const [days, updateDays] = React.useReducer(daysStateReducer, {
    allowed: null,
    rangeMillis: null,
    endType: "same-day",
  });
  const [startTime, setStartTime] = React.useState<any>(
    new Date("1970-01-01T00:00:00"),
  );
  const [endTime, setEndTime] = React.useState<any>(null);

  useEffect(
    () => updateDays({ op: "update-selected-streams", selectedStreams }),
    [selectedStreams],
  );
  const shouldDisableDate = (date: Date | null) => {
    return (
      days.allowed === null ||
      !days.allowed.allMillis.has(startOfDay(date!).getTime())
    );
  };

  // Update range90k to reflect the selected options.
  useEffect(() => {
    if (days.rangeMillis === null) {
      setRange90k(null);
      return;
    }
    const start = combine(days.rangeMillis[0], startTime);
    const end = combine(days.rangeMillis[1], endTime);
    setRange90k([
      zonedTimeToUtc(start, timeZoneName).getTime() * 90,
      zonedTimeToUtc(end, timeZoneName).getTime() * 90,
    ]);
  }, [days, startTime, endTime, timeZoneName, setRange90k]);

  const today = new Date();

  let startDate = null;
  let endDate = null;
  if (days.rangeMillis !== null) {
    startDate = new Date(days.rangeMillis[0]);
    endDate = new Date(days.rangeMillis[1]);
  }
  return (
    <Paper sx={{ padding: theme.spacing(1) }}>
      <Box>
        <FormLabel component="legend">From</FormLabel>
        <SmallStaticDatePicker
          displayStaticWrapperAs="desktop"
          value={startDate}
          disabled={days.allowed === null}
          shouldDisableDate={shouldDisableDate}
          maxDate={
            days.allowed === null ? today : new Date(days.allowed.maxMillis)
          }
          minDate={
            days.allowed === null ? today : new Date(days.allowed.minMillis)
          }
          onChange={(d: Date | null) => {
            updateDays({ op: "set-start-day", newStartDate: d });
          }}
        />
        <MyTimePicker
          value={startTime}
          onChange={(newValue) => {
            if (newValue === null || isFinite((newValue as Date).getTime())) {
              setStartTime(newValue);
            }
          }}
          disabled={days.allowed === null}
        />
      </Box>
      <Box>
        <FormLabel sx={{ mt: 1 }} component="legend">
          To
        </FormLabel>
        <RadioGroup
          row
          value={days.endType}
          onChange={(e) => {
            updateDays({
              op: "set-end-type",
              newEndType: e.target.value as EndDayType,
            });
          }}
        >
          <FormControlLabel
            value="same-day"
            control={<Radio size="small" color="secondary" />}
            label="Same day"
          />
          <FormControlLabel
            value="other-day"
            control={<Radio size="small" color="secondary" />}
            label="Other day"
          />
        </RadioGroup>
        <Collapse in={days.endType === "other-day"}>
          <SmallStaticDatePicker
            displayStaticWrapperAs="desktop"
            value={endDate}
            shouldDisableDate={(d: Date | null) =>
              days.endType !== "other-day" || shouldDisableDate(d)
            }
            maxDate={
              startDate === null ? today : new Date(days.allowed!.maxMillis)
            }
            minDate={startDate === null ? today : startDate}
            onChange={(d: Date | null) => {
              updateDays({ op: "set-end-day", newEndDate: d! });
            }}
          />
        </Collapse>
        <MyTimePicker
          value={endTime}
          onChange={(newValue) => {
            if (newValue === null || isFinite((newValue as Date).getTime())) {
              setEndTime(newValue);
            }
          }}
          disabled={days.allowed === null}
        />
      </Box>
    </Paper>
  );
};

export default TimerangeSelector;
