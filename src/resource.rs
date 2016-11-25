// This file is part of Moonfire NVR, a security camera digital video recorder.
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

extern crate core;
extern crate hyper;
extern crate time;

use error::Result;
use hyper::server::{Request, Response};
use hyper::header;
use hyper::method::Method;
use hyper::net::Fresh;
use mime;
use smallvec::SmallVec;
use std::cmp;
use std::io;
use std::ops::Range;

/// An HTTP resource for GET and HEAD serving.
pub trait Resource {
    /// Returns the length of the slice in bytes.
    fn len(&self) -> u64;

    /// Writes bytes within this slice indicated by `range` to `out.`
    /// TODO: different result type?
    fn write_to(&self, range: Range<u64>, out: &mut io::Write) -> Result<()>;

    fn content_type(&self) -> mime::Mime;
    fn etag(&self) -> Option<&header::EntityTag>;
    fn last_modified(&self) -> &header::HttpDate;
}

#[derive(Debug, Eq, PartialEq)]
enum ResolvedRanges {
    AbsentOrInvalid,
    NotSatisfiable,
    Satisfiable(SmallVec<[Range<u64>; 1]>)
}

fn parse_range_header(range: Option<&header::Range>, resource_len: u64) -> ResolvedRanges {
    if let Some(&header::Range::Bytes(ref byte_ranges)) = range {
        let mut ranges: SmallVec<[Range<u64>; 1]> = SmallVec::new();
        for range in byte_ranges {
            match *range {
                header::ByteRangeSpec::FromTo(range_from, range_to) => {
                    let end = cmp::min(range_to + 1, resource_len);
                    if range_from >= end {
                        debug!("Range {:?} not satisfiable with length {:?}", range, resource_len);
                        continue;
                    }
                    ranges.push(Range{start: range_from, end: end});
                },
                header::ByteRangeSpec::AllFrom(range_from) => {
                    if range_from >= resource_len {
                        debug!("Range {:?} not satisfiable with length {:?}", range, resource_len);
                        continue;
                    }
                    ranges.push(Range{start: range_from, end: resource_len});
                },
                header::ByteRangeSpec::Last(last) => {
                    if last >= resource_len {
                        debug!("Range {:?} not satisfiable with length {:?}", range, resource_len);
                        continue;
                    }
                    ranges.push(Range{start: resource_len - last,
                                      end: resource_len});
                },
            }
        }
        if !ranges.is_empty() {
            debug!("Ranges {:?} all satisfiable with length {:?}", range, resource_len);
            return ResolvedRanges::Satisfiable(ranges);
        }
        return ResolvedRanges::NotSatisfiable;
    }
    ResolvedRanges::AbsentOrInvalid
}

/// Returns true if `req` doesn't have an `If-None-Match` header matching `req`.
fn none_match(etag: Option<&header::EntityTag>, req: &Request) -> bool {
    match req.headers.get::<header::IfNoneMatch>() {
        Some(&header::IfNoneMatch::Any) => false,
        Some(&header::IfNoneMatch::Items(ref items)) => {
            if let Some(some_etag) = etag {
                for item in items {
                    if item.weak_eq(some_etag) {
                        return false;
                    }
                }
            }
            true
        },
        None => true,
    }
}

/// Returns true if `req` has no `If-Match` header or one which matches `etag`.
fn any_match(etag: Option<&header::EntityTag>, req: &Request) -> bool {
    match req.headers.get::<header::IfMatch>() {
        Some(&header::IfMatch::Any) => true,
        Some(&header::IfMatch::Items(ref items)) => {
            if let Some(some_etag) = etag {
                for item in items {
                    if item.strong_eq(some_etag) {
                        return true;
                    }
                }
            }
            false
        },
        None => false,
    }
}

/// Serves GET and HEAD requests for a given byte-ranged resource.
/// Handles conditional & subrange requests.
/// The caller is expected to have already determined the correct resource and appended
/// Expires, Cache-Control, and Vary headers.
///
/// TODO: is it appropriate to include those headers on all response codes used in this function?
///
/// TODO: check HTTP rules about weak vs strong comparisons with range requests. I don't think I'm
/// doing this correctly.
pub fn serve(rsrc: &Resource, req: &Request, mut res: Response<Fresh>) -> Result<()> {
    if req.method != Method::Get && req.method != Method::Head {
        *res.status_mut() = hyper::status::StatusCode::MethodNotAllowed;
        res.headers_mut().set(header::ContentType(mime!(Text/Plain)));
        res.headers_mut().set(header::Allow(vec![Method::Get, Method::Head]));
        res.send(b"This resource only supports GET and HEAD.")?;
        return Ok(());
    }

    let last_modified = rsrc.last_modified();
    let etag = rsrc.etag();
    res.headers_mut().set(header::AcceptRanges(vec![header::RangeUnit::Bytes]));
    res.headers_mut().set(header::LastModified(*last_modified));
    if let Some(some_etag) = etag {
        res.headers_mut().set(header::ETag(some_etag.clone()));
    }

    if let Some(&header::IfUnmodifiedSince(ref since)) = req.headers.get() {
        if last_modified.0.to_timespec() > since.0.to_timespec() {
            *res.status_mut() = hyper::status::StatusCode::PreconditionFailed;
            res.send(b"Precondition failed")?;
            return Ok(());
        }
    }

    if any_match(etag, req) {
        *res.status_mut() = hyper::status::StatusCode::PreconditionFailed;
        res.send(b"Precondition failed")?;
        return Ok(());
    }

    if !none_match(etag, req) {
        *res.status_mut() = hyper::status::StatusCode::NotModified;
        res.send(b"")?;
        return Ok(());
    }

    if let Some(&header::IfModifiedSince(ref since)) = req.headers.get() {
        if last_modified <= since {
            *res.status_mut() = hyper::status::StatusCode::NotModified;
            res.send(b"")?;
            return Ok(());
        }
    }

    let mut range_hdr = req.headers.get::<header::Range>();

    // See RFC 2616 section 10.2.7: a Partial Content response should include certain
    // entity-headers or not based on the If-Range response.
    let include_entity_headers_on_range = match req.headers.get::<header::IfRange>() {
        Some(&header::IfRange::EntityTag(ref if_etag)) => {
            if let Some(some_etag) = etag {
                if if_etag.strong_eq(some_etag) {
                    false
                } else {
                    range_hdr = None;
                    true
                }
            } else {
                range_hdr = None;
                true
            }
        },
        Some(&header::IfRange::Date(ref if_date)) => {
            // The to_timespec conversion appears necessary because in the If-Range off the wire,
            // fields such as tm_yday are absent, causing strict equality to spuriously fail.
            if if_date.0.to_timespec() != last_modified.0.to_timespec() {
                range_hdr = None;
                true
            } else {
                false
            }
        },
        None => true,
    };
    let len = rsrc.len();
    let (range, include_entity_headers) = match parse_range_header(range_hdr, len) {
        ResolvedRanges::AbsentOrInvalid => (0 .. len, true),
        ResolvedRanges::Satisfiable(rs) => {
            if rs.len() == 1 {
                res.headers_mut().set(header::ContentRange(
                    header::ContentRangeSpec::Bytes{
                        range: Some((rs[0].start, rs[0].end-1)),
                        instance_length: Some(len)}));
                *res.status_mut() = hyper::status::StatusCode::PartialContent;
                (rs[0].clone(), include_entity_headers_on_range)
            } else {
                // Ignore multi-part range headers for now. They require additional complexity, and
                // I don't see clients sending them in the wild.
                (0 .. len, true)
            }
        },
        ResolvedRanges::NotSatisfiable => {
            res.headers_mut().set(header::ContentRange(
                header::ContentRangeSpec::Bytes{
                    range: None,
                    instance_length: Some(len)}));
            *res.status_mut() = hyper::status::StatusCode::RangeNotSatisfiable;
            res.send(b"")?;
            return Ok(());
        }
    };
    if include_entity_headers {
        res.headers_mut().set(header::ContentType(rsrc.content_type()));
    }
    res.headers_mut().set(header::ContentLength(range.end - range.start));
    let mut stream = res.start()?;
    if req.method == Method::Get {
        rsrc.write_to(range, &mut stream)?;
    }
    stream.end()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use error::Result;
    use hyper;
    use hyper::header::{self, ByteRangeSpec, ContentRangeSpec, EntityTag};
    use hyper::header::Range::Bytes;
    use mime;
    use smallvec::SmallVec;
    use std::io::{Read, Write};
    use std::ops::Range;
    use std::sync::Mutex;
    use super::{ResolvedRanges, parse_range_header};
    use super::*;
    use testutil;
    use time;

    /// Tests the specific examples enumerated in RFC 2616 section 14.35.1.
    #[test]
    fn test_resolve_ranges_rfc() {
        let mut v = SmallVec::new();

        v.push(0 .. 500);
        assert_eq!(ResolvedRanges::Satisfiable(v.clone()),
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::FromTo(0, 499)])),
                                      10000));

        v.clear();
        v.push(500 .. 1000);
        assert_eq!(ResolvedRanges::Satisfiable(v.clone()),
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::FromTo(500, 999)])),
                                      10000));

        v.clear();
        v.push(9500 .. 10000);
        assert_eq!(ResolvedRanges::Satisfiable(v.clone()),
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::Last(500)])),
                                      10000));

        v.clear();
        v.push(9500 .. 10000);
        assert_eq!(ResolvedRanges::Satisfiable(v.clone()),
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::AllFrom(9500)])),
                                      10000));

        v.clear();
        v.push(0 .. 1);
        v.push(9999 .. 10000);
        assert_eq!(ResolvedRanges::Satisfiable(v.clone()),
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::FromTo(0, 0),
                                                              ByteRangeSpec::Last(1)])),
                                      10000));

        // Non-canonical ranges. Possibly the point of these is that the adjacent and overlapping
        // ranges are supposed to be coalesced into one? I'm not going to do that for now.

        v.clear();
        v.push(500 .. 601);
        v.push(601 .. 1000);
        assert_eq!(ResolvedRanges::Satisfiable(v.clone()),
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::FromTo(500, 600),
                                                              ByteRangeSpec::FromTo(601, 999)])),
                                      10000));

        v.clear();
        v.push(500 .. 701);
        v.push(601 .. 1000);
        assert_eq!(ResolvedRanges::Satisfiable(v.clone()),
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::FromTo(500, 700),
                                                              ByteRangeSpec::FromTo(601, 999)])),
                                      10000));
    }

    #[test]
    fn test_resolve_ranges_satisfiability() {
        assert_eq!(ResolvedRanges::NotSatisfiable,
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::AllFrom(10000)])),
                                      10000));

        let mut v = SmallVec::new();
        v.push(0 .. 500);
        assert_eq!(ResolvedRanges::Satisfiable(v.clone()),
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::FromTo(0, 499),
                                                              ByteRangeSpec::AllFrom(10000)])),
                                      10000));

        assert_eq!(ResolvedRanges::NotSatisfiable,
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::Last(1)])), 0));
        assert_eq!(ResolvedRanges::NotSatisfiable,
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::FromTo(0, 0)])), 0));
        assert_eq!(ResolvedRanges::NotSatisfiable,
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::AllFrom(0)])), 0));

        v.clear();
        v.push(0 .. 1);
        assert_eq!(ResolvedRanges::Satisfiable(v.clone()),
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::FromTo(0, 0)])), 1));

        v.clear();
        v.push(0 .. 500);
        assert_eq!(ResolvedRanges::Satisfiable(v.clone()),
                   parse_range_header(Some(&Bytes(vec![ByteRangeSpec::FromTo(0, 10000)])),
                                      500));
    }

    #[test]
    fn test_resolve_ranges_absent_or_invalid() {
        assert_eq!(ResolvedRanges::AbsentOrInvalid, parse_range_header(None, 10000));
    }

    struct FakeResource {
        etag: Option<EntityTag>,
        mime: mime::Mime,
        last_modified: header::HttpDate,
        body: &'static [u8],
    }

    impl Resource for FakeResource {
        fn len(&self) -> u64 { self.body.len() as u64 }
        fn write_to(&self, range: Range<u64>, out: &mut Write) -> Result<()> {
            Ok(out.write_all(&self.body[range.start as usize .. range.end as usize])?)
        }
        fn content_type(&self) -> mime::Mime { self.mime.clone() }
        fn etag(&self) -> Option<&EntityTag> { self.etag.as_ref() }
        fn last_modified(&self) -> &header::HttpDate { &self.last_modified }
    }

    fn new_server() -> String {
        let mut listener = hyper::net::HttpListener::new("127.0.0.1:0").unwrap();
        use hyper::net::NetworkListener;
        let addr = listener.local_addr().unwrap();
        let server = hyper::Server::new(listener);
        use std::thread::spawn;
        spawn(move || {
            use hyper::server::{Request, Response, Fresh};
            let _ = server.handle(move |req: Request, res: Response<Fresh>| {
                let l = RESOURCE.lock().unwrap();
                let resource = l.as_ref().unwrap();
                serve(resource, &req, res).unwrap();
            });
        });
        format!("http://{}:{}/", addr.ip(), addr.port())
    }

    lazy_static! {
        static ref RESOURCE: Mutex<Option<FakeResource>> = { Mutex::new(None) };
        static ref SERVER: String = { new_server() };
        static ref SOME_DATE: header::HttpDate = {
            header::HttpDate(time::at_utc(time::Timespec::new(1430006400i64, 0)))
        };
        static ref LATER_DATE: header::HttpDate = {
            header::HttpDate(time::at_utc(time::Timespec::new(1430092800i64, 0)))
        };
    }

    #[test]
    fn serve_without_etag() {
        testutil::init_logging();
        *RESOURCE.lock().unwrap() = Some(FakeResource{
            etag: None,
            mime: mime!(Application/OctetStream),
            last_modified: *SOME_DATE,
            body: b"01234",
        });
        let client = hyper::Client::new();
        let mut buf = Vec::new();

        // Full body.
        let mut resp = client.get(&*SERVER).send().unwrap();
        assert_eq!(hyper::status::StatusCode::Ok, resp.status);
        assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
                   resp.headers.get::<header::ContentType>());
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"01234", &buf[..]);

        // If-None-Match any.
        let mut resp = client.get(&*SERVER)
                             .header(header::IfNoneMatch::Any)
                             .send()
                             .unwrap();
        assert_eq!(hyper::status::StatusCode::NotModified, resp.status);
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"", &buf[..]);

        // If-None-Match by etag doesn't match (as this request has no etag).
        let mut resp =
            client.get(&*SERVER)
                  .header(header::IfNoneMatch::Items(vec![EntityTag::strong("foo".to_owned())]))
                  .send()
                  .unwrap();
        assert_eq!(hyper::status::StatusCode::Ok, resp.status);
        assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
                   resp.headers.get::<header::ContentType>());
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"01234", &buf[..]);

        // Unmodified since supplied date.
        let mut resp = client.get(&*SERVER)
                             .header(header::IfModifiedSince(*SOME_DATE))
                             .send()
                             .unwrap();
        assert_eq!(hyper::status::StatusCode::NotModified, resp.status);
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"", &buf[..]);

        // Range serving - basic case.
        let mut resp = client.get(&*SERVER)
                             .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
                             .send()
                             .unwrap();
        assert_eq!(hyper::status::StatusCode::PartialContent, resp.status);
        assert_eq!(Some(&header::ContentRange(ContentRangeSpec::Bytes{
            range: Some((1, 3)),
            instance_length: Some(5),
        })), resp.headers.get());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"123", &buf[..]);

        // Range serving - multiple ranges. Currently falls back to whole range.
        let mut resp = client.get(&*SERVER)
                             .header(Bytes(vec![ByteRangeSpec::FromTo(0, 1),
                                                ByteRangeSpec::FromTo(3, 4)]))
                             .send()
                             .unwrap();
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        assert_eq!(hyper::status::StatusCode::Ok, resp.status);
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"01234", &buf[..]);

        // Range serving - not satisfiable.
        let mut resp = client.get(&*SERVER)
                             .header(Bytes(vec![ByteRangeSpec::AllFrom(500)]))
                             .send()
                             .unwrap();
        assert_eq!(hyper::status::StatusCode::RangeNotSatisfiable, resp.status);
        assert_eq!(Some(&header::ContentRange(ContentRangeSpec::Bytes{
            range: None,
            instance_length: Some(5),
        })), resp.headers.get());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"", &buf[..]);

        // Range serving - matching If-Range by date honors the range.
        let mut resp = client.get(&*SERVER)
                             .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
                             .header(header::IfRange::Date(*SOME_DATE))
                             .send()
                             .unwrap();
        assert_eq!(hyper::status::StatusCode::PartialContent, resp.status);
        assert_eq!(Some(&header::ContentRange(ContentRangeSpec::Bytes{
            range: Some((1, 3)),
            instance_length: Some(5),
        })), resp.headers.get());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"123", &buf[..]);

        // Range serving - non-matching If-Range by date ignores the range.
        let mut resp = client.get(&*SERVER)
                             .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
                             .header(header::IfRange::Date(*LATER_DATE))
                             .send()
                             .unwrap();
        assert_eq!(hyper::status::StatusCode::Ok, resp.status);
        assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
                   resp.headers.get::<header::ContentType>());
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"01234", &buf[..]);

        // Range serving - this resource has no etag, so any If-Range by etag ignores the range.
        let mut resp =
            client.get(&*SERVER)
                  .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
                  .header(header::IfRange::EntityTag(EntityTag::strong("foo".to_owned())))
                  .send()
                  .unwrap();
        assert_eq!(hyper::status::StatusCode::Ok, resp.status);
        assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
                   resp.headers.get::<header::ContentType>());
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"01234", &buf[..]);
    }

    #[test]
    fn serve_with_strong_etag() {
        testutil::init_logging();
        *RESOURCE.lock().unwrap() = Some(FakeResource{
            etag: Some(EntityTag::strong("foo".to_owned())),
            mime: mime!(Application/OctetStream),
            last_modified: *SOME_DATE,
            body: b"01234",
        });
        let client = hyper::Client::new();
        let mut buf = Vec::new();

        // If-None-Match by etag which matches.
        let mut resp =
            client.get(&*SERVER)
                  .header(header::IfNoneMatch::Items(vec![EntityTag::strong("foo".to_owned())]))
                  .send()
                  .unwrap();
        assert_eq!(hyper::status::StatusCode::NotModified, resp.status);
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"", &buf[..]);

        // If-None-Match by etag which doesn't match.
        let mut resp =
            client.get(&*SERVER)
                  .header(header::IfNoneMatch::Items(vec![EntityTag::strong("bar".to_owned())]))
                  .send()
                  .unwrap();
        assert_eq!(hyper::status::StatusCode::Ok, resp.status);
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"01234", &buf[..]);

        // Range serving - If-Range matching by etag.
        let mut resp =
            client.get(&*SERVER)
                  .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
                  .header(header::IfRange::EntityTag(EntityTag::strong("foo".to_owned())))
                  .send()
                  .unwrap();
        assert_eq!(hyper::status::StatusCode::PartialContent, resp.status);
        assert_eq!(None, resp.headers.get::<header::ContentType>());
        assert_eq!(Some(&header::ContentRange(ContentRangeSpec::Bytes{
            range: Some((1, 3)),
            instance_length: Some(5),
        })), resp.headers.get());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"123", &buf[..]);

        // Range serving - If-Range not matching by etag.
        let mut resp =
            client.get(&*SERVER)
                  .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
                  .header(header::IfRange::EntityTag(EntityTag::strong("bar".to_owned())))
                  .send()
                  .unwrap();
        assert_eq!(hyper::status::StatusCode::Ok, resp.status);
        assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
                   resp.headers.get::<header::ContentType>());
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"01234", &buf[..]);
    }

    #[test]
    fn serve_with_weak_etag() {
        testutil::init_logging();
        *RESOURCE.lock().unwrap() = Some(FakeResource{
            etag: Some(EntityTag::weak("foo".to_owned())),
            mime: mime!(Application/OctetStream),
            last_modified: *SOME_DATE,
            body: b"01234",
        });
        let client = hyper::Client::new();
        let mut buf = Vec::new();

        // If-None-Match by identical weak etag is sufficient.
        let mut resp =
            client.get(&*SERVER)
                  .header(header::IfNoneMatch::Items(vec![EntityTag::weak("foo".to_owned())]))
                  .send()
                  .unwrap();
        assert_eq!(hyper::status::StatusCode::NotModified, resp.status);
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"", &buf[..]);

        // If-None-Match by etag which doesn't match.
        let mut resp =
            client.get(&*SERVER)
                  .header(header::IfNoneMatch::Items(vec![EntityTag::weak("bar".to_owned())]))
                  .send()
                  .unwrap();
        assert_eq!(hyper::status::StatusCode::Ok, resp.status);
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"01234", &buf[..]);

        // Range serving - If-Range matching by weak etag isn't sufficient.
        let mut resp =
            client.get(&*SERVER)
                  .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
                  .header(header::IfRange::EntityTag(EntityTag::weak("foo".to_owned())))
                  .send()
                  .unwrap();
        assert_eq!(hyper::status::StatusCode::Ok, resp.status);
        assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
                   resp.headers.get::<header::ContentType>());
        assert_eq!(None, resp.headers.get::<header::ContentRange>());
        buf.clear();
        resp.read_to_end(&mut buf).unwrap();
        assert_eq!(b"01234", &buf[..]);
    }
}
