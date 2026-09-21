# SPDX-License-Identifier: Apache-2.0
"""A model from nothing with IfcOpenShell: project, units, contexts, site, building and storeys."""

from __future__ import annotations

from contextlib import contextmanager
from pathlib import Path


@contextmanager
def owner_scope(model, producer: str = "tessifc"):
    """While active, the IfcOpenShell API records the model's owner on every rooted entity.

    IFC2X3 requires an owner history with a user and an application; the API reads them
    from module-level hooks, which are restored on exit. Other schemas are left alone.
    """
    import ifcopenshell.api.owner.settings as owner_settings
    from ifcopenshell import api

    if model.schema != "IFC2X3":
        yield
        return
    user = next(iter(model.by_type("IfcPersonAndOrganization")), None)
    application = next(iter(model.by_type("IfcApplication")), None)
    if user is None:
        person = api.run("owner.add_person", model, identification="", family_name="", given_name=producer)
        organisation = api.run("owner.add_organisation", model, identification="", name=producer)
        user = api.run("owner.add_person_and_organisation", model, person=person, organisation=organisation)
    if application is None:
        developer = user.TheOrganization
        application = api.run("owner.add_application", model, version="0.2", application_full_name=producer,
                              application_identifier=producer, application_developer=developer)
    hooks = (owner_settings.get_user, owner_settings.get_application)
    owner_settings.get_user = lambda ifc: user
    owner_settings.get_application = lambda ifc: application
    try:
        yield
    finally:
        owner_settings.get_user, owner_settings.get_application = hooks


def create_model(schema: str = "IFC4", *, name: str = "New project", units: str = "m", site: str = "Site",
                 building: str = "Building", storeys=None, producer: str = "tessifc"):
    """The same skeleton `createModel` writes in JavaScript, as an `ifcopenshell.file`."""
    import ifcopenshell
    from ifcopenshell import api

    schema = str(schema).upper()
    if schema not in ("IFC2X3", "IFC4", "IFC4X3"):
        raise ValueError(f"Unsupported schema {schema}; use IFC2X3, IFC4 or IFC4X3")
    if units not in ("m", "mm"):
        raise ValueError(f"Unsupported length unit {units}; use m or mm")
    storeys = list(storeys or [{"name": "Ground floor", "elevation": 0.0}])
    model = ifcopenshell.file(schema=schema)
    with owner_scope(model, producer):
        _populate(model, api, name=name, units=units, site=site, building=building, storeys=storeys)
    try:
        model.wrapped_data.header.file_name.originating_system = producer
    except (AttributeError, TypeError):
        # The header helper differs between IfcOpenShell versions; the producer name is optional.
        pass
    return model


def _populate(model, api, *, name, units, site, building, storeys):
    project = api.run("root.create_entity", model, ifc_class="IfcProject", name=name)
    length = api.run("unit.add_si_unit", model, unit_type="LENGTHUNIT", prefix="MILLI" if units == "mm" else None)
    area = api.run("unit.add_si_unit", model, unit_type="AREAUNIT")
    volume = api.run("unit.add_si_unit", model, unit_type="VOLUMEUNIT")
    api.run("unit.assign_unit", model, units=[length, area, volume])
    context = api.run("context.add_context", model, context_type="Model")
    api.run("context.add_context", model, context_type="Model", context_identifier="Body", target_view="MODEL_VIEW", parent=context)
    site_entity = api.run("root.create_entity", model, ifc_class="IfcSite", name=site)
    building_entity = api.run("root.create_entity", model, ifc_class="IfcBuilding", name=building)
    api.run("aggregate.assign_object", model, relating_object=project, products=[site_entity])
    api.run("aggregate.assign_object", model, relating_object=site_entity, products=[building_entity])
    api.run("geometry.edit_object_placement", model, product=site_entity)
    api.run("geometry.edit_object_placement", model, product=building_entity)
    for index, storey in enumerate(storeys):
        elevation = float(storey.get("elevation", 0.0) or 0.0)
        entity = api.run("root.create_entity", model, ifc_class="IfcBuildingStorey", name=storey.get("name") or f"Storey {index + 1}")
        entity.Elevation = elevation
        api.run("aggregate.assign_object", model, relating_object=building_entity, products=[entity])
        matrix = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, elevation], [0.0, 0.0, 0.0, 1.0]]
        api.run("geometry.edit_object_placement", model, product=entity, matrix=matrix)


def write_model(model, path) -> Path:
    """Write a model with an atomic replacement, so a follower never reads half a file."""
    import os
    import tempfile

    target = Path(path).resolve()
    fd, name = tempfile.mkstemp(prefix=".tessifc-", suffix=".ifc", dir=target.parent)
    os.close(fd)
    model.write(name)
    Path(name).replace(target)
    return target
