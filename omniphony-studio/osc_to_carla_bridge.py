#!/usr/bin/env python3
"""
OSC Bridge: gsrd → Carla/SPARTA Panner
Maps immersive object positions to Carla plugin parameters via OSC

Usage:
    python osc_to_carla_bridge.py [--config mapping.yaml]

Install dependencies:
    pip install python-osc pyyaml

Carla OSC Format:
    /Carla/<plugin_id>/set_parameter_value <parameter_id> <value>
    Example: /Carla/0/set_parameter_value 3 0.75
"""

import argparse
import yaml
from pythonosc import udp_client
from pythonosc.dispatcher import Dispatcher
from pythonosc.osc_server import BlockingOSCUDPServer


class OSCBridge:
    def __init__(self, config_file, carla_host="127.0.0.1", carla_port=22752):
        # Load mapping configuration
        with open(config_file, 'r') as f:
            self.config = yaml.safe_load(f)

        # Create OSC client to send to Carla
        self.carla_client = udp_client.SimpleUDPClient(carla_host, carla_port)

        print(f"Connected to Carla OSC: {carla_host}:{carla_port}")
        print(f"Loaded mapping config: {config_file}")

        self.object_count = 0
        self.frame_count = 0
        self.source_count = None

    def send_to_carla(self, plugin_id, param_id, value):
        """Send parameter change to Carla"""
        # Carla OSC format: /Carla/<plugin_id>/set_parameter_value <param_id> <value>
        address = f"/Carla/{plugin_id}/set_parameter_value"
        self.carla_client.send_message(
            address,
            [param_id, value]
        )

    def cartesian_to_polar(self, x, y, z):
        """Convert cartesian coordinates to spherical/polar coordinates

        Args:
            x, y, z: Cartesian coordinates in range [-1, +1]

        Returns:
            (azimuth_degrees, elevation_degrees, distance)
            - azimuth: -180° to +180° (0° = front, -90° = left, ±180° = back, +90° = right)
            - elevation: -90° to +90° (0° = horizontal, +90° = up, -90° = down)
            - distance: 0-1 (normalized radius)
        """
        import math

        # Distance (radius)
        distance = math.sqrt(x*x + y*y + z*z)

        # Azimuth (horizontal angle)
        # atan2(x, y) returns angle in [-π, π] which is [-180°, +180°]
        azimuth_rad = math.atan2(x, y)
        azimuth_deg = math.degrees(azimuth_rad)

        # Elevation (vertical angle)
        if distance > 0.0:
            elevation_deg = math.degrees(math.asin(z / distance))
        else:
            elevation_deg = 0.0

        return (azimuth_deg, elevation_deg, distance)

    def normalize_value(self, value, input_range, output_range):
        """Normalize value from input range to output range"""
        in_min, in_max = input_range
        out_min, out_max = output_range

        # Normalize to 0-1
        normalized = (value - in_min) / (in_max - in_min)
        normalized = max(0.0, min(1.0, normalized))

        # Map to output range
        return out_min + normalized * (out_max - out_min)

    def object_position_handler(self, address, *args):
        """Handle /omniphony/object/{id}/xyz messages"""
        # Extract object_id from address like /omniphony/object/0/xyz
        parts = address.split('/')
        if len(parts) >= 4 and len(args) >= 3:
            try:
                object_id = int(parts[3])
                x, y, z = args[0], args[1], args[2]

                # Extended metadata (optional, from args[3] onwards)
                gain = args[3] if len(args) > 3 else -128
                priority = args[4] if len(args) > 4 else 0.0
                divergence = args[5] if len(args) > 5 else 0.0
                ramp_duration = args[6] if len(args) > 6 else 0

                # Spread parameters (new, from args[7] onwards)
                size_x = args[7] if len(args) > 7 else 0.0
                size_y = args[8] if len(args) > 8 else 0.0
                size_z = args[9] if len(args) > 9 else 0.0
                zone_constraints = args[10] if len(args) > 10 else 0
                distance_factor = args[11] if len(args) > 11 else 0
                screen_factor = args[12] if len(args) > 12 else 0.0
                depth_factor = args[13] if len(args) > 13 else 0.25

                # Display object info in compact format (4 lines per object)

                # Convert cartesian to polar coordinates
                azimuth, elevation, distance = self.cartesian_to_polar(x, y, z)

                # Get mapping for this object
                mapping = self.config.get('objects', {}).get(object_id)
                if not mapping:
                    # No mapping for this object, skip
                    return

                plugin_id = mapping['plugin_id']
                carla_params = []

                # Map azimuth
                if 'azimuth' in mapping:
                    param = mapping['azimuth']
                    value = self.normalize_value(
                        azimuth,
                        param.get('input_range', [0, 360]),
                        param.get('output_range', [0, 1])
                    )
                    self.send_to_carla(plugin_id, param['param_id'], value)
                    carla_params.append(f"P{param['param_id']}")

                # Map elevation
                if 'elevation' in mapping:
                    param = mapping['elevation']
                    value = self.normalize_value(
                        elevation,
                        param.get('input_range', [-90, 90]),
                        param.get('output_range', [0, 1])
                    )
                    self.send_to_carla(plugin_id, param['param_id'], value)
                    carla_params.append(f"P{param['param_id']}")

                # Map distance
                if 'distance' in mapping:
                    param = mapping['distance']
                    value = self.normalize_value(
                        distance,
                        param.get('input_range', [0, 1]),
                        param.get('output_range', [0, 1])
                    )
                    self.send_to_carla(plugin_id, param['param_id'], value)
                    carla_params.append(f"P{param['param_id']}")

                # Build Carla mapping info
                carla_info = f"→ Carla[{plugin_id}].{'/'.join(carla_params)}" if carla_params else ""

                # Print object info (4 lines: position, audio, spread, separator)
                print(f"Obj{object_id + 1:2d}: Pos({x:+.2f},{y:+.2f},{z:+.2f})  Az={azimuth:6.1f}°  El={elevation:+5.1f}°  Dist={distance:.2f}  {carla_info}")
                print(f"      Audio: Gain={gain:+3d}dB  Priority={priority:5.1f}  Ramp={ramp_duration:5d} samples")
                print(f"      Spread: Div={divergence:.3f}  Size=[{size_x:.3f},{size_y:.3f},{size_z:.3f}]  Zone={zone_constraints}  Dist={distance_factor}  Screen={screen_factor:.3f}  Depth={depth_factor:.3f}")
                print(f"{'-'*80}")

            except (ValueError, KeyError) as e:
                print(f"Error mapping object: {e}")

    def frame_handler(self, address, *args):
        """Handle /omniphony/spatial/frame messages"""
        if len(args) >= 2:
            sample_pos, object_count = args
            if object_count != self.object_count:
                self.object_count = object_count
                print(f"\n[Frame {self.frame_count}] Objects: {object_count}")
            self.frame_count += 1

    def source_config_handler(self, address, *args):
        """Handle /omniphony/source/config messages"""
        if len(args) >= 1:
            source_count = args[0]

            # Print only if value changed
            if self.source_count != source_count:
                print(f"\n[Source Configuration] Total sources: {source_count}")
                print(f"  This stream has {source_count} audio sources (bed channels + dynamic objects)")
                self.source_count = source_count

            # Always send to Carla (even if value unchanged) in case of decoder reset
            source_config = self.config.get('source_count_mapping')
            if source_config:
                plugin_id = source_config.get('plugin_id')
                param_id = source_config.get('param_id')

                if plugin_id is not None and param_id is not None:
                    # Normalize source count if output range is specified
                    if 'output_range' in source_config:
                        value = self.normalize_value(
                            source_count,
                            source_config.get('input_range', [1, 128]),  # SPARTA range: 1-128
                            source_config.get('output_range', [0, 1])
                        )
                    else:
                        # Send raw source count
                        value = float(source_count)

                    self.send_to_carla(plugin_id, param_id, value)
                    print(f"  → Sent to Carla[{plugin_id}].{param_id} = {value}")


def create_default_config():
    """Create a default mapping configuration file"""
    config = {
        'carla': {
            'host': '127.0.0.1',
            'port': 22752,
            'description': 'Carla OSC server (default port)'
        },
        'source_count_mapping': {
            'plugin_id': 0,  # Plugin ID in Carla
            'param_id': 8,   # Parameter ID for number of sources (typically param 8 for SPARTA)
            'input_range': [1, 128],  # SPARTA range: 1-128 (not 0-128!)
            'output_range': [0, 1],   # Output range (0.0 to 1.0 for normalized)
            'description': 'Mapping for total source count (bed + dynamic objects)'
        },
        'objects': {
            0: {
                'plugin_id': 0,  # SPARTA Panner plugin ID in Carla
                'azimuth': {
                    'param_id': 0,  # Parameter ID for azimuth
                    'input_range': [0, 360],
                    'output_range': [0, 1]
                },
                'elevation': {
                    'param_id': 1,  # Parameter ID for elevation
                    'input_range': [-90, 90],
                    'output_range': [0, 1]
                },
                'distance': {
                    'param_id': 2,  # Parameter ID for distance
                    'input_range': [0, 1],
                    'output_range': [0, 1]
                }
            },
            # Add more objects as needed
            # 1: { ... },
            # 2: { ... },
        }
    }

    with open('carla_mapping.yaml', 'w') as f:
        yaml.dump(config, f, default_flow_style=False, sort_keys=False)

    print("Created default configuration: carla_mapping.yaml")
    print("\nEdit this file to map objects to your Carla plugin parameters:")
    print("1. Find your plugin ID in Carla (hover over plugin)")
    print("2. Find parameter IDs (right-click parameter → 'Edit')")
    print("3. Update the source_count_mapping to set the number of sources (param 8)")
    print("4. Update object mappings for azimuth, elevation, distance")
    print("\nThe source_count_mapping sends the total number of audio sources")
    print("(bed channels + dynamic objects) to Carla parameter 8.")
    print("SPARTA Panner supports up to 128 input channels.")


def main():
    parser = argparse.ArgumentParser(description='OSC bridge: gsrd → Carla')
    parser.add_argument('--config', default='carla_mapping.yaml',
                       help='Mapping configuration file (default: carla_mapping.yaml)')
    parser.add_argument('--create-config', action='store_true',
                       help='Create default configuration file and exit')
    parser.add_argument('--gsrd-port', type=int, default=9000,
                       help='Port to receive OSC from gsrd (default: 9000)')
    args = parser.parse_args()

    if args.create_config:
        create_default_config()
        return

    # Load configuration
    try:
        # Read config file to get Carla host/port
        with open(args.config, 'r') as f:
            config = yaml.safe_load(f)

        carla_config = config.get('carla', {})
        carla_host = carla_config.get('host', '127.0.0.1')
        carla_port = carla_config.get('port', 22752)

        bridge = OSCBridge(
            args.config,
            carla_host=carla_host,
            carla_port=carla_port
        )
    except FileNotFoundError:
        print(f"Error: Configuration file '{args.config}' not found")
        print("Create one with: python osc_to_carla_bridge.py --create-config")
        return
    except Exception as e:
        print(f"Error loading configuration: {e}")
        return

    # Create OSC dispatcher for gsrd messages
    dispatcher = Dispatcher()
    dispatcher.map("/omniphony/object/*/xyz", bridge.object_position_handler)
    dispatcher.map("/omniphony/spatial/frame", bridge.frame_handler)
    dispatcher.map("/omniphony/source/config", bridge.source_config_handler)

    # Create and start OSC server
    server = BlockingOSCUDPServer(("127.0.0.1", args.gsrd_port), dispatcher)

    print("=" * 70)
    print("gsrd → Carla OSC Bridge")
    print("=" * 70)
    print(f"Listening for gsrd OSC: 127.0.0.1:{args.gsrd_port}")
    print(f"Sending to Carla: {carla_host}:{carla_port}")
    print()
    print("Message flow:")
    print("  gsrd → OSC (port 9000) → This bridge → Carla OSC → SPARTA Panner")
    print()
    print("Press Ctrl+C to stop")
    print("=" * 70)
    print()

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\n\nBridge stopped.")


if __name__ == '__main__':
    main()
