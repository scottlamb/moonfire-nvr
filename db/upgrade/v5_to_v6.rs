// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors
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

/// Upgrades a version 4 schema to a version 5 schema.

use byteorder::{BigEndian, ByteOrder, WriteBytesExt};
use failure::{Error, bail, format_err};
use h264_reader::avcc::AvcDecoderConfigurationRecord;
use rusqlite::{named_params, params};
use std::convert::{TryFrom, TryInto};

// Copied from src/h264.rs. h264 stuff really doesn't belong in the db crate, but we do what we
// must for schema upgrades.
const PIXEL_ASPECT_RATIOS: [((u16, u16), (u16, u16)); 4] = [
    ((320, 240), ( 4,  3)),
    ((352, 240), (40, 33)),
    ((640, 480), ( 4,  3)),
    ((704, 480), (40, 33)),
];
fn default_pixel_aspect_ratio(width: u16, height: u16) -> (u16, u16) {
    let dims = (width, height);
    for r in &PIXEL_ASPECT_RATIOS {
        if r.0 == dims {
            return r.1;
        }
    }
    (1, 1)
}

fn parse(data: &[u8]) -> Result<AvcDecoderConfigurationRecord, Error> {
    if data.len() < 94 || &data[4..8] != b"avc1" || &data[90..94] != b"avcC" {
        bail!("data of len {} doesn't have an avcC", data.len());
    }
    let avcc_len = BigEndian::read_u32(&data[86..90]);
    if avcc_len < 8 { // length and type.
        bail!("invalid avcc len {}", avcc_len);
    }
    let end_pos = 86 + usize::try_from(avcc_len)?;
    if end_pos != data.len() {
        bail!("expected avcC to be end of extradata; there are {} more bytes.",
              data.len() - end_pos);
    }
    AvcDecoderConfigurationRecord::try_from(&data[94..end_pos])
        .map_err(|e| format_err!("Bad AvcDecoderConfigurationRecord: {:?}", e))
}

pub fn run(_args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    // These create statements match the schema.sql when version 5 was the latest.
    tx.execute_batch(r#"
        alter table video_sample_entry rename to old_video_sample_entry;

        create table video_sample_entry (
          id integer primary key,
          width integer not null check (width > 0),
          height integer not null check (height > 0),
          rfc6381_codec text not null,
          data blob not null check (length(data) > 86),
          pasp_h_spacing integer not null default 1 check (pasp_h_spacing > 0),
          pasp_v_spacing integer not null default 1 check (pasp_v_spacing > 0)
        );
    "#)?;

    let mut insert = tx.prepare(r#"
        insert into video_sample_entry (id,  width,  height,  rfc6381_codec,  data,
                                        pasp_h_spacing,  pasp_v_spacing)
                                values (:id, :width, :height, :rfc6381_codec, :data,
                                        :pasp_h_spacing, :pasp_v_spacing)
    "#)?;
    let mut stmt = tx.prepare(r#"
        select
          id,
          width,
          height,
          rfc6381_codec,
          data
        from
          old_video_sample_entry
    "#)?;
    let mut rows = stmt.query(params![])?;
    while let Some(row) = rows.next()? {
        let id: i32 = row.get(0)?;
        let width: u16 = row.get::<_, i32>(1)?.try_into()?;
        let height: u16 = row.get::<_, i32>(2)?.try_into()?;
        let rfc6381_codec: &str = row.get_raw_checked(3)?.as_str()?;
        let mut data: Vec<u8> = row.get(4)?;
        let avcc = parse(&data)?;
        if avcc.num_of_sequence_parameter_sets() != 1 {
            bail!("Multiple SPSs!");
        }
        let ctx = avcc.create_context(())
            .map_err(|e| format_err!("Can't load SPS+PPS: {:?}", e))?;
        let sps = ctx.sps_by_id(h264_reader::nal::pps::ParamSetId::from_u32(0).unwrap())
            .ok_or_else(|| format_err!("No SPS 0"))?;
        let pasp = sps.vui_parameters.as_ref()
                                     .and_then(|v| v.aspect_ratio_info.as_ref())
                                     .and_then(|a| a.clone().get())
                                     .unwrap_or_else(|| default_pixel_aspect_ratio(width, height));
        if pasp != (1, 1) {
            data.extend_from_slice(b"\x00\x00\x00\x10pasp");  // length + box name
            data.write_u32::<BigEndian>(pasp.0.into())?;
            data.write_u32::<BigEndian>(pasp.1.into())?;
            let len = data.len();
            BigEndian::write_u32(&mut data[0..4], u32::try_from(len)?);
        }

        insert.execute_named(named_params!{
            ":id": id,
            ":width": width,
            ":height": height,
            ":rfc6381_codec": rfc6381_codec,
            ":data": &data,
            ":pasp_h_spacing": pasp.0,
            ":pasp_v_spacing": pasp.1,
        })?;
    }
    tx.execute_batch(r#"
        alter table stream rename to old_stream;
        create table stream (
          id integer primary key,
          camera_id integer not null references camera (id),
          sample_file_dir_id integer references sample_file_dir (id),
          type text not null check (type in ('main', 'sub')),
          record integer not null check (record in (1, 0)),
          rtsp_url text not null,
          retain_bytes integer not null check (retain_bytes >= 0),
          flush_if_sec integer not null,
          cum_recordings integer not null check (cum_recordings >= 0),
          cum_duration_90k integer not null check (cum_duration_90k >= 0),
          cum_runs integer not null check (cum_runs >= 0),
          unique (camera_id, type)
        );
        insert into stream
        select
          s.id,
          s.camera_id,
          s.sample_file_dir_id,
          s.type,
          s.record,
          s.rtsp_url,
          s.retain_bytes,
          s.flush_if_sec,
          s.next_recording_id as cum_recordings,
          coalesce(sum(r.duration_90k), 0) as cum_duration_90k,
          coalesce(sum(case when r.run_offset = 0 then 1 else 0 end), 0) as cum_runs
        from
          old_stream s left join recording r on (s.id = r.stream_id)
        group by 1;

        alter table recording rename to old_recording;
        create table recording (
          composite_id integer primary key,
          open_id integer not null,
          stream_id integer not null references stream (id),
          run_offset integer not null,
          flags integer not null,
          sample_file_bytes integer not null check (sample_file_bytes > 0),
          start_time_90k integer not null check (start_time_90k > 0),
          prev_duration_90k integer not null check (prev_duration_90k >= 0),
          prev_runs integer not null check (prev_runs >= 0),
          duration_90k integer not null
              check (duration_90k >= 0 and duration_90k < 5*60*90000),
          video_samples integer not null check (video_samples > 0),
          video_sync_samples integer not null check (video_sync_samples > 0),
          video_sample_entry_id integer references video_sample_entry (id),
          check (composite_id >> 32 = stream_id)
        );
    "#)?;

    // SQLite added window functions in 3.25.0. macOS still ships SQLite 3.24.0 (no support).
    // Compute cumulative columns by hand.
    let mut cur_stream_id = None;
    let mut cum_duration_90k = 0;
    let mut cum_runs = 0;
    let mut stmt = tx.prepare(r#"
        select
          composite_id,
          open_id,
          stream_id,
          run_offset,
          flags,
          sample_file_bytes,
          start_time_90k,
          duration_90k,
          video_samples,
          video_sync_samples,
          video_sample_entry_id
        from
          old_recording
        order by composite_id
    "#)?;
    let mut insert = tx.prepare(r#"
        insert into recording (composite_id, open_id, stream_id, run_offset, flags,
                               sample_file_bytes, start_time_90k, prev_duration_90k, prev_runs,
                               duration_90k, video_samples, video_sync_samples,
                               video_sample_entry_id)
                       values (:composite_id, :open_id, :stream_id, :run_offset, :flags,
                               :sample_file_bytes, :start_time_90k, :prev_duration_90k, :prev_runs,
                               :duration_90k, :video_samples, :video_sync_samples,
                               :video_sample_entry_id)
    "#)?;
    let mut rows = stmt.query(params![])?;
    while let Some(row) = rows.next()? {
        let composite_id: i64 = row.get(0)?;
        let open_id: i32 = row.get(1)?;
        let stream_id: i32 = row.get(2)?;
        let run_offset: i32 = row.get(3)?;
        let flags: i32 = row.get(4)?;
        let sample_file_bytes: i32 = row.get(5)?;
        let start_time_90k: i64 = row.get(6)?;
        let duration_90k: i32 = row.get(7)?;
        let video_samples: i32 = row.get(8)?;
        let video_sync_samples: i32 = row.get(9)?;
        let video_sample_entry_id: i32 = row.get(10)?;
        if cur_stream_id != Some(stream_id) {
            cum_duration_90k = 0;
            cum_runs = 0;
            cur_stream_id = Some(stream_id);
        }
        insert.execute_named(named_params!{
            ":composite_id": composite_id,
            ":open_id": open_id,
            ":stream_id": stream_id,
            ":run_offset": run_offset,
            ":flags": flags,
            ":sample_file_bytes": sample_file_bytes,
            ":start_time_90k": start_time_90k,
            ":prev_duration_90k": cum_duration_90k,
            ":prev_runs": cum_runs,
            ":duration_90k": duration_90k,
            ":video_samples": video_samples,
            ":video_sync_samples": video_sync_samples,
            ":video_sample_entry_id": video_sample_entry_id,
        })?;
        cum_duration_90k += duration_90k;
        cum_runs += if run_offset == 0 { 1 } else { 0 };
    }
    tx.execute_batch(r#"
        drop index recording_cover;
        create index recording_cover on recording (
          stream_id,
          start_time_90k,
          open_id,
          duration_90k,
          video_samples,
          video_sync_samples,
          video_sample_entry_id,
          sample_file_bytes,
          run_offset,
          flags
        );

        alter table recording_integrity rename to old_recording_integrity;
        create table recording_integrity (
          composite_id integer primary key references recording (composite_id),
          local_time_delta_90k integer,
          local_time_since_open_90k integer,
          wall_time_delta_90k integer,
          sample_file_blake3 blob check (length(sample_file_blake3) <= 32)
        );
        insert into recording_integrity
        select
          composite_id,
          local_time_delta_90k,
          local_time_since_open_90k,
          wall_time_delta_90k,
          null
        from
          old_recording_integrity;

        alter table recording_playback rename to old_recording_playback;
        create table recording_playback (
          composite_id integer primary key references recording (composite_id),
          video_index blob not null check (length(video_index) > 0)
        );
        insert into recording_playback select * from old_recording_playback;

        drop table old_recording_playback;
        drop table old_recording_integrity;
        drop table old_recording;
        drop table old_stream;
        drop table old_video_sample_entry;

        update user_session
        set
          revocation_reason = 1,
          revocation_reason_detail = 'Blake2b->Blake3 upgrade'
        where
          revocation_reason is null;

        create table object_detection_model (
          id integer primary key,
          uuid blob unique not null check (length(uuid) = 16),
          name text not null,
          data blob
        );

        create table object_detection_label (
          id integer primary key,
          uuid blob unique not null check (length(uuid) = 16),
          name text unique not null,
          color text
        );

        create table recording_object_detection (
          composite_id integer not null references recording (composite_id),
          model_id integer not null references object_detection_model (id),
          frame_data blob not null,
          primary key (composite_id, model_id)
        );
    "#)?;
    Ok(())
}
