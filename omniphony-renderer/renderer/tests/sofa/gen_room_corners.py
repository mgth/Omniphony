#!/usr/bin/env python3
"""Regenerate the room_corners_*.sofa fixtures beside this script.

Requires Python's standard library and shared HDF5 >= 1.10 libraries
libhdf5/libhdf5_hl (the Debian libhdf5_serial names also work).
Neither h5py nor netCDF4 is needed. The base
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
    # Older libraries use 32-bit hid_t: check before binding any handle API
    # or reading the exported type/property-list IDs with in_dll.
    version = (C.c_uint(), C.c_uint(), C.c_uint())
    bind(hdf, "H5get_libversion", C.c_int, *([C.POINTER(C.c_uint)] * 3))(
        *(C.byref(part) for part in version))
    version = tuple(part.value for part in version)
    if version < (1, 10, 0):
        raise RuntimeError(f"HDF5 >= 1.10 is required; found {'.'.join(map(str, version))}")
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
    space_npoints = bind(hdf, "H5Sget_simple_extent_npoints", C.c_int64, hid)
    dataset_create = bind(hdf, "H5Dcreate2", hid, hid, string, hid, hid, hid, hid, hid)
    dataset_open = bind(hdf, "H5Dopen2", hid, hid, string, hid)
    dataset_close = bind(hdf, "H5Dclose", integer, hid)
    dataset_space = bind(hdf, "H5Dget_space", hid, hid)
    dataset_write = bind(hdf, "H5Dwrite", integer, hid, hid, hid, hid, hid, C.c_void_p)
    attach_scale = bind(hl, "H5DSattach_scale", integer, hid, hid, C.c_uint)
    attribute = bind(hl, "H5LTset_attribute_string", integer, hid, string, string, string)
    attribute_open = bind(hdf, "H5Aopen", hid, hid, string, hid)
    attribute_close = bind(hdf, "H5Aclose", integer, hid)
    attribute_space = bind(hdf, "H5Aget_space", hid, hid)
    attribute_type = bind(hdf, "H5Aget_type", hid, hid)
    attribute_write = bind(hdf, "H5Awrite", integer, hid, hid, C.c_void_p)
    type_close = bind(hdf, "H5Tclose", integer, hid)
    type_class = bind(hdf, "H5Tget_class", integer, hid)
    type_size = bind(hdf, "H5Tget_size", C.c_size_t, hid)
    variable_string = bind(hdf, "H5Tis_variable_str", integer, hid)
    native_double = hid.in_dll(hdf, "H5T_NATIVE_DOUBLE_g").value
    file_double = hid.in_dll(hdf, "H5T_IEEE_F64LE_g").value
    dataset_properties = hid.in_dll(hdf, "H5P_CLS_DATASET_CREATE_ID_g").value

    def room_type(file):
        # Keep the original attribute's storage. Deleting/recreating it with
        # H5LTset_attribute_string leaves a gap in the dense attribute heap;
        # sofar's current heap reader then misses the attributes after it.
        with ExitStack() as stack:
            attr = attribute_open(file, b"RoomType", 0)
            stack.callback(attribute_close, attr)
            space = attribute_space(attr)
            stack.callback(space_close, space)
            kind = attribute_type(attr)
            stack.callback(type_close, kind)
            if (space_npoints(space) != 1 or type_class(kind) != 3
                    or variable_string(kind)):  # H5T_STRING, fixed length
                raise ValueError("RoomType must be a scalar fixed-length string")
            width = type_size(kind)
            if width < len(b"shoebox") + 1:
                raise ValueError("RoomType has no space for a terminated 'shoebox'")
            attribute_write(attr, kind, C.create_string_buffer(b"shoebox", width))

    def write(dataset, values):
        space = dataset_space(dataset)
        try:
            count = space_npoints(space)
            if count != len(values):
                raise ValueError(f"Refusing H5Dwrite: dataset has {count} elements, "
                                 f"buffer has {len(values)}")
        finally:
            space_close(space)
        dataset_write(dataset, native_double, 0, 0, 0,
                      (C.c_double * len(values))(*values))

    def dataset(file, name, shape, values, dimensions):
        if math.prod(shape) != len(values) or len(shape) != len(dimensions):
            raise ValueError(f"{name!r}: shape, values and dimensions disagree")
        with ExitStack() as stack:
            props = prop_create(dataset_properties)
            stack.callback(prop_close, props)
            track_times(props, 0)
            space = space_create(len(shape), (size * len(shape))(*shape), None)
            stack.callback(space_close, space)
            data = dataset_create(file, name, file_double, space, 0, props, 0)
            stack.callback(dataset_close, data)
            write(data, values)
            for axis, dimension in enumerate(dimensions):
                scale = dataset_open(file, dimension, 0)
                stack.callback(dataset_close, scale)
                attach_scale(data, scale, axis)

    def metadata(file, name, spherical, units, prefix=b""):
        attribute(file, name, prefix + b"Type", b"spherical" if spherical else b"cartesian")
        attribute(file, name, prefix + b"Units", units)

    def spherical_corner(corner):
        x, y, z = corner
        return (math.degrees(math.atan2(y, x)),
                math.degrees(math.atan2(z, math.hypot(x, y))),
                math.sqrt(x * x + y * y + z * z))

    directory = Path(__file__).resolve().parent
    # name, spherical encoding, metadata location, unsupported units,
    # listener offset. Only the last case changes any existing dataset.
    cases = (
        ("cartesian", False, "shared", False, None),
        ("spherical", True, "shared", False, None),
        ("own_metadata", True, "corner", False, None),
        ("global_metadata", True, "global", False, None),
        ("unsupported_unit", False, "shared", True, None),
        ("offset_listener", False, "shared", False, (3.0, 2.0, 1.2)),
    )
    for name, spherical, location, unsupported_unit, listener in cases:
        path = directory / f"room_corners_{name}.sofa"
        shutil.copyfile(directory / "chunked_multispeaker_brir.sofa", path)
        with ExitStack() as stack:
            file = file_open(bytes(path), 1, 0)  # H5F_ACC_RDWR, default properties
            stack.callback(file_close, file)
            room_type(file)
            units = b"degree, degree, metre" if spherical else b"metre"
            if unsupported_unit:
                units = b"foot"
            for variable, corner in (
                (b"RoomCornerA", (0.0, 0.0, 0.0)),
                (b"RoomCornerB", (6.0, 4.0, 2.5)),
            ):
                values = spherical_corner(corner) if spherical else corner
                dataset(file, variable, (1, 3), values, (b"I", b"C"))
                if location == "corner":
                    metadata(file, variable, spherical, units)
            if location == "shared":
                # The convention's otherwise unused variable carries the
                # encoding; neither corner nor global attributes duplicate it.
                dataset(file, b"RoomCorners", (1, 1), (0.0,), (b"I", b"I"))
                metadata(file, b"RoomCorners", spherical, units)
            elif location == "global":
                metadata(file, b"/", spherical, units, prefix=b"RoomCorners:")
            if listener is not None:
                data = dataset_open(file, b"ListenerPosition", 0)
                stack.callback(dataset_close, data)
                write(data, listener)
        print(f"wrote {path.name} ({path.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
