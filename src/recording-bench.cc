// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016 Scott Lamb <slamb@slamb.org>
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
//
// recording-bench.cc: benchmarks of the recording.h interface.

#include <benchmark/benchmark.h>
#include <gflags/gflags.h>

#include "recording.h"
#include "testutil.h"

DECLARE_bool(alsologtostderr);

static void BM_Iterator(benchmark::State &state) {
  using moonfire_nvr::ReadFileOrDie;
  using moonfire_nvr::SampleIndexIterator;
  // state.PauseTiming();
  std::string index = ReadFileOrDie("../src/testdata/video_sample_index.bin");
  // state.ResumeTiming();
  while (state.KeepRunning()) {
    SampleIndexIterator it(index);
    while (!it.done()) it.Next();
    CHECK(!it.has_error()) << it.error();
  }
  state.SetBytesProcessed(int64_t(state.iterations()) * int64_t(index.size()));
}
BENCHMARK(BM_Iterator);

int main(int argc, char **argv) {
  FLAGS_alsologtostderr = true;

  // Sadly, these two flag-parsing libraries don't appear to get along.
  // google::ParseCommandLineFlags(&argc, &argv, true);
  benchmark::Initialize(&argc, argv);

  google::InitGoogleLogging(argv[0]);
  benchmark::RunSpecifiedBenchmarks();
  return 0;
}
