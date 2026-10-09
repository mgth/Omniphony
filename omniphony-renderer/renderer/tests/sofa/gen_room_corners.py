#!/usr/bin/env python3
"""Regenerate the room_corners_*.sofa fixtures beside this script.

Requires Python's standard library and shared libhdf5/libhdf5_hl (the Debian
libhdf5_serial names also work). Neither h5py nor netCDF4 is needed. The base
file's responses and geometry are kept, except for the offset-listener case.
New datasets do not store creation times, so rerunning with the same HDF5
version produces identical files.
"""

import ctypes as C
from ctypes.util import find_library
from contextlib import ExitStack
import math
from pathlib import Path
import shutil


def libraries():
    for name in ("hdf5", "hdf5_serial"):
        core, high = find_library(name), find_library(name + "_hl")
        if core and high:
            return C.CDLL(core), C.CDLL(high)
    raise RuntimeError("Install shared libhdf5 and libhdf5_hl to regenerate fixtures")


def checked(result, function, _args):
    if result < 0:
        raise RuntimeError(f"{function.__name__} failed")
    return result


def bind(lib, name, result, *args):
    function = getattr(lib, name)
    function.restype = result
    function.argtypes = args
    function.errcheck = checked
    return function


def main():
    hdf, hl = libraries()
    hid = C.c_int64
    size = C.c_uint64
    integer = C.c_int
    string = C.c_char_p
    bind(hdf, "H5open", integer)()

    file_open = bind(hdf, "H5Fopen", hid, string, C.c_uint, hid)
    file_close = bind(hdf, "H5Fclose", integer, hid)
    prop_create = bind(hdf, "H5Pcreate", hid, hid)
    prop_close = bind(hdf, "H5Pclose", integer, hid)
    track_times = bind(hdf, "H5Pset_obj_track_times", integer, hid, C.c_uint)
    space_create = bind(hdf, "H5Screate_simple", hid, integer,
                        C.POINTER(size), C.POINTER(size))
    space_close = bind(hdf, "H5Sclose", integer, hid)
    dataset_create = bind(hdf, "H5Dcreate2", hid, hid, string, hid, hid, hid, hid, hid)
    dataset_open = bind(hdf, "H5Dopen2", hid, hid, string, hid)
    dataset_close = bind(hdf, "H5Dclose", integer, hid)
    dataset_write = bind(hdf, "H5Dwrite", integer, hid, hid, hid, hid, hid, C.c_void_p)
    attribute = bind(hl, "H5LTset_attribute_string", integer, hid, string, string, string)
    native_double = hid.in_dll(hdf, "H5T_NATIVE_DOUBLE_g").value
    file_double = hid.in_dll(hdf, "H5T_IEEE_F64LE_g").value
    dataset_properties = hid.in_dll(hdf, "H5P_CLS_DATASET_CREATE_ID_g").value

    def write(dataset, values):
        dataset_write(dataset, native_double, 0, 0, 0,
                      (C.c_double * len(values))(*values))

    def dataset(file, name, shape, values):
        assert math.prod(shape) == len(values)
        with ExitStack() as stack:
            props = prop_create(dataset_properties)
            stack.callback(prop_close, props)
            track_times(props, 0)
            space = space_create(len(shape), (size * len(shape))(*shape), None)
            stack.callback(space_close, space)
            data = dataset_create(file, name, file_double, space, 0, props, 0)
            stack.callback(dataset_close, data)
            write(data, values)

    def metadata(file, name, spherical, units):
        attribute(file, name, b"Type", b"spherical" if spherical else b"cartesian")
        attribute(file, name, b"Units", units)

    def spherical_corner(corner):
        x, y, z = corner
        return (math.degrees(math.atan2(y, x)),
                math.degrees(math.atan2(z, math.hypot(x, y))),
                math.sqrt(x * x + y * y + z * z))

    directory = Path(__file__).resolve().parent
    # name, spherical encoding, metadata on each corner, unsupported units,
    # listener offset. Only the last case changes any existing dataset.
    cases = (
        ("cartesian", False, False, False, None),
        ("spherical", True, False, False, None),
        ("own_metadata", True, True, False, None),
        ("unsupported_unit", False, False, True, None),
        ("offset_listener", False, False, False, (3.0, 2.0, 1.2)),
    )
    for name, spherical, own_metadata, unsupported_unit, listener in cases:
        path = directory / f"room_corners_{name}.sofa"
        shutil.copyfile(directory / "chunked_multispeaker_brir.sofa", path)
        with ExitStack() as stack:
            file = file_open(bytes(path), 1, 0)  # H5F_ACC_RDWR, default properties
            stack.callback(file_close, file)
            attribute(file, b"/", b"RoomType", b"shoebox")
            units = b"degree, degree, metre" if spherical else b"metre"
            if unsupported_unit:
                units = b"foot"
            for variable, corner in (
                (b"RoomCornerA", (0.0, 0.0, 0.0)),
                (b"RoomCornerB", (6.0, 4.0, 2.5)),
            ):
                values = spherical_corner(corner) if spherical else corner
                dataset(file, variable, (1, 3), values)  # [I][C]
                if own_metadata:
                    metadata(file, variable, spherical, units)
            if not own_metadata:
                # The convention's otherwise unused variable carries the
                # encoding; neither corner nor global attributes duplicate it.
                dataset(file, b"RoomCorners", (1,), (0.0,))
                metadata(file, b"RoomCorners", spherical, units)
            if listener is not None:
                data = dataset_open(file, b"ListenerPosition", 0)
                stack.callback(dataset_close, data)
                write(data, listener)
        print(f"wrote {path.name} ({path.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
