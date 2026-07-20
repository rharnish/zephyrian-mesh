#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Created on Sun Jul 12 22:25:35 2026

@author: rharnish
"""

from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware
import xarray as xr
import numpy as np

#%%

"""
TO START SERVER:

(base) rharnish@kennebec:~/Desktop/ERA5_19780608$ conda activate reginald
(reginald) rharnish@kennebec:~/Desktop/ERA5_19780608$ uvicorn get_wind_data:app --reload
"""

# netcdf_file = "data/4bd6338f235b60709d9140c9abfa0ade/data_stream-oper_stepType-instant.nc"
netcdf_file = "data/67e68497a30d3b5946ac20a319b63fd4.nc"


D = {}

#%%

app = FastAPI()

# Enable CORS for React frontend
app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_methods=["*"],
    allow_headers=["*"],
)

@app.get("/api/wind-data")
def get_wind_data():
    # 1. Open NetCDF file
    ds = xr.open_dataset(netcdf_file)
    
    
    # 2. Select specific slice (e.g., first time step, lowest pressure level)
    # Adjust variable names ('u10', 'v10', 'latitude', 'longitude') to match your file
    # latest_data = ds.isel(time=0)
    latest_data = ds.isel(valid_time=0)
    
    # 3. Downsample if data is too dense for the browser (every 2nd or 3rd grid point)
    sampled = latest_data.isel(latitude=slice(None, None, 2), longitude=slice(None, None, 2))
    
    # 4. Extract raw matrices
    u_vals = np.nan_to_num(sampled['u10'].values).tolist()
    v_vals = np.nan_to_num(sampled['v10'].values).tolist()
    lats = sampled['latitude'].values.tolist()
    lons = sampled['longitude'].values.tolist()
    
    # 5. Format payload (matches standard wind particle input formats)
    payload = {
        "header": {
            "nx": len(lons),
            "ny": len(lats),
            "lo1": min(lons),
            "la1": max(lats), # Typically grid starts top-left (max lat)
            "lo2": max(lons),
            "la2": min(lats),
            "dx": abs(lons[1] - lons[0]) if len(lons) > 1 else 1,
            "dy": abs(lats[1] - lats[0]) if len(lats) > 1 else 1
        },
        "u_data": u_vals,
        "v_data": v_vals
    }
    
    return payload

#%%

import matplotlib.pyplot as plt

r = get_wind_data()

plt.imshow(np.array(r['u_data']))
plt.show()

plt.imshow(np.array(r['v_data']))
plt.show()



#%%

grib_file = "data/728a51f77540a02326c03790fb282b7e.grib"