// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { Stream } from "../types";
import StaticDatePicker, {
  StaticDatePickerProps,
} from "@material-ui/lab/StaticDatePicker";
import React, { useEffect } from "react";
import { zonedTimeToUtc } from "date-fns-tz";
import { addDays, addMilliseconds, differenceInMilliseconds } from "date-fns";
import startOfDay from "date-fns/startOfDay";
import Card from "@material-ui/core/Card";
import { useTheme } from "@material-ui/core/styles";
import TextField from "@material-ui/core/TextField";
import FormControlLabel from "@material-ui/core/FormControlLabel";
import FormLabel from "@material-ui/core/FormLabel";
import Radio from "@material-ui/core/Radio";
import RadioGroup from "@material-ui/core/RadioGroup";
import TimePicker, { TimePickerProps } from "@material-ui/lab/TimePicker";
import Collapse from "@material-ui/core/Collapse";
import Box from "@material-ui/core/Box";

interface Props {
  selectedStreams: Set<Stream>;
  timeZoneName: string;
  range90k: [number, number] | null;
  setRange90k: (range: [number, number] | null) => void;
}

const MyTimePicker = (
  props: Pick<TimePickerProps, "value" | "onChange" | "disabled">
) => (
  <TimePicker
    label="Time"
    views={["hours", "minutes", "seconds"]}
    renderInput={(params) => <TextField fullWidth size="small" {...params} />}
    inputFormat="HH:mm:ss"
    mask="__:__:__"
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
          "& > div": {
            minWidth: 256,
          },
          "& > div > div, & > div > div > div, & .MuiCalendarPicker-root": {
            width: 256,
          },
          "& .MuiTypography-caption": {
            width: DATE_SIZE,
            margin: 0,
          },
          "& .PrivatePickersSlideTransition-root": {
            minHeight: DATE_SIZE * 6,
          },
          '& .PrivatePickersSlideTransition-root [role="row"]': {
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
      <StaticDatePicker {...props} />
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
    differenceInMilliseconds(time, startOfDay(time))
  );
};

/**
 * Allowed days to select (ones with video).
 *
 * These are stored in a funny format: number of milliseconds since epoch of
 * the start of the given day in the browser's time zone. This is because
 * (a) Date objects are always in the local time zone and date-fn rolls with
 * that, and (b) Date objects don't work well in a set. Javascript's
 * "same-value-zero algorithm" means that two different Date objects never
 * compare the same.
 */
type AllowedDays = {
  minMillis: number;
  maxMillis: number;
  allMillis: Set<number>;
};

type EndDayType = "same-day" | "other-day";

type DaysState = {
  allowed: AllowedDays | null;

  /** [start, end] in same format as described for <tt>AllowedDays</tt>. */
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
  selectedStreams: Set<Stream>
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
    case "set-end-day":
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
  range90k,
  setRange90k,
}: Props) => {
  const theme = useTheme();
  const [days, updateDays] = React.useReducer(daysStateReducer, {
    allowed: null,
    rangeMillis: null,
    endType: "same-day",
  });
  const [startTime, setStartTime] = React.useState<any>(
    new Date("1970-01-01T00:00:00")
  );
  const [endTime, setEndTime] = React.useState<any>(null);

  useEffect(
    () => updateDays({ op: "update-selected-streams", selectedStreams }),
    [selectedStreams]
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
    <Card sx={{ padding: theme.spacing(1) }}>
      <div>
        <FormLabel component="legend">From</FormLabel>
        <SmallStaticDatePicker
          displayStaticWrapperAs="desktop"
          value={startDate}
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
          renderInput={(params) => <TextField {...params} variant="outlined" />}
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
      </div>
      <div>
        <FormLabel component="legend">To</FormLabel>
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
            renderInput={(params) => (
              <TextField {...params} variant="outlined" />
            )}
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
      </div>
    </Card>
  );
};

export default TimerangeSelector;
